use std::fs;

use seaf_core::LoopStepName;

use crate::{
    state::step_file_stem,
    workspace::{
        write_artifact, LoopWorkspace, WorkspaceError, ARTIFACTS_DIR, PROMPTS_DIR, RESPONSES_DIR,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactContent {
    extension: String,
    bytes: Vec<u8>,
}

impl ArtifactContent {
    pub fn new(extension: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        let extension = normalize_extension(extension.into());
        Self {
            extension,
            bytes: bytes.into(),
        }
    }

    pub fn markdown(content: impl Into<String>) -> Self {
        Self::new("md", content.into().into_bytes())
    }

    pub fn extension(&self) -> &str {
        &self.extension
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub fn write_step_request(
    workspace: &LoopWorkspace,
    step: LoopStepName,
    attempt: u32,
    request: &str,
) -> Result<String, WorkspaceError> {
    let relative_path = format!(
        "{}/{}",
        PROMPTS_DIR,
        request_file_name(step_file_stem(step), attempt)
    );
    write_artifact(
        workspace.run_directory(),
        &relative_path,
        request.as_bytes(),
    )?;
    Ok(relative_path)
}

pub fn write_step_response(
    workspace: &LoopWorkspace,
    step: LoopStepName,
    attempt: u32,
    response: &str,
) -> Result<String, WorkspaceError> {
    let relative_path = format!(
        "{}/{}",
        RESPONSES_DIR,
        response_file_name(step_file_stem(step), attempt)
    );
    write_artifact(
        workspace.run_directory(),
        &relative_path,
        response.as_bytes(),
    )?;
    Ok(relative_path)
}

pub fn write_step_artifact(
    workspace: &LoopWorkspace,
    step: LoopStepName,
    artifact: &ArtifactContent,
) -> Result<String, WorkspaceError> {
    let relative_path = format!(
        "{}/{}.{}",
        ARTIFACTS_DIR,
        step_file_stem(step),
        artifact.extension()
    );
    write_artifact(workspace.run_directory(), &relative_path, artifact.bytes())?;
    Ok(relative_path)
}

pub fn next_step_attempt(
    workspace: &LoopWorkspace,
    step: LoopStepName,
) -> Result<u32, WorkspaceError> {
    let prompts_dir = workspace.run_directory().join(PROMPTS_DIR);
    let stem = step_file_stem(step);
    let canonical_name = request_file_name(stem.clone(), 1);
    let mut highest_attempt = 0;

    if !prompts_dir.exists() {
        return Ok(1);
    }

    for entry in fs::read_dir(&prompts_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();

        let attempt = if file_name == canonical_name {
            Some(1)
        } else {
            attempt_from_request_file_name(&stem, &file_name)
        };

        if let Some(attempt) = attempt {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(WorkspaceError::UnsafeExistingLayout(
                    entry.path(),
                    "prompt attempt is not a real regular file or is a symlink".to_string(),
                ));
            }
            highest_attempt = highest_attempt.max(attempt);
        }
    }

    highest_attempt.checked_add(1).ok_or_else(|| {
        WorkspaceError::UnsafeExistingLayout(
            prompts_dir,
            format!("prompt attempt sequence is exhausted for {stem}; start a new run"),
        )
    })
}

fn request_file_name(stem: String, attempt: u32) -> String {
    if attempt == 1 {
        format!("{stem}.prompt.md")
    } else {
        format!("{stem}.attempt-{attempt:03}.prompt.md")
    }
}

fn response_file_name(stem: String, attempt: u32) -> String {
    if attempt == 1 {
        format!("{stem}.raw.txt")
    } else {
        format!("{stem}.attempt-{attempt:03}.raw.txt")
    }
}

fn attempt_from_request_file_name(stem: &str, file_name: &str) -> Option<u32> {
    let attempt = file_name
        .strip_prefix(stem)?
        .strip_prefix(".attempt-")?
        .strip_suffix(".prompt.md")?;
    attempt.parse().ok()
}

fn normalize_extension(extension: String) -> String {
    let extension = extension.trim().trim_start_matches('.');
    if extension.is_empty()
        || !extension.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return "bin".to_string();
    }

    extension.to_ascii_lowercase()
}
