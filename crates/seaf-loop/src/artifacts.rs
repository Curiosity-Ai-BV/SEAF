use std::{collections::BTreeSet, fs, path::Path};

use seaf_core::LoopStepName;
use sha2::{Digest, Sha256};

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

    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(&self.bytes))
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
    let absolute = workspace.run_directory().join(&relative_path);
    match fs::symlink_metadata(&absolute) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(WorkspaceError::UnsafeExistingLayout(
                absolute,
                "prompt attempt is not a real regular file or is a symlink".to_string(),
            ));
        }
        Ok(_) => {
            if fs::read(&absolute)? != request.as_bytes() {
                return Err(WorkspaceError::UnsafeExistingLayout(
                    absolute,
                    "existing prompt attempt differs from the recovered request bytes".to_string(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_artifact(
                workspace.run_directory(),
                &relative_path,
                request.as_bytes(),
            )?;
        }
        Err(error) => return Err(error.into()),
    }
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
    attempt: u32,
    artifact: &ArtifactContent,
) -> Result<String, WorkspaceError> {
    let authoritative_attempt = latest_step_attempt(workspace, step)?;
    if attempt == 0 || authoritative_attempt != Some(attempt) {
        return Err(WorkspaceError::UnsafeExistingLayout(
            workspace.run_directory().join(PROMPTS_DIR),
            format!(
                "role artifact attempt {attempt} does not match durable prompt attempt authority {authoritative_attempt:?}"
            ),
        ));
    }
    refuse_ambiguous_fixed_pointer(workspace, step, attempt)?;
    let file_name = if attempt == 1 {
        format!("{}.{}", step_file_stem(step), artifact.extension())
    } else {
        format!(
            "{}.attempt-{attempt:03}.{}",
            step_file_stem(step),
            artifact.extension()
        )
    };
    let relative_path = format!("{ARTIFACTS_DIR}/{file_name}");
    validate_role_artifact_attempt_slot(workspace, step, attempt, &file_name)?;
    crate::immutable_artifact::publish_create_only(
        workspace.run_directory(),
        &relative_path,
        artifact.bytes(),
    )
    .map_err(|error| {
        WorkspaceError::UnsafeExistingLayout(
            workspace.run_directory().join(&relative_path),
            error.to_string(),
        )
    })?;
    Ok(relative_path)
}

fn refuse_ambiguous_fixed_pointer(
    workspace: &LoopWorkspace,
    step: LoopStepName,
    attempt: u32,
) -> Result<(), WorkspaceError> {
    if attempt < 2 {
        return Ok(());
    }
    let run_path = workspace.run_file();
    match fs::symlink_metadata(&run_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(WorkspaceError::UnsafeExistingLayout(
                run_path,
                "run authority is not a real regular file".to_string(),
            ));
        }
        Ok(_) => {}
    }
    let bytes = crate::immutable_artifact::read_verified_regular_file(
        workspace.run_directory(),
        crate::workspace::RUN_FILE,
        "role artifact run authority",
    )
    .map_err(|error| WorkspaceError::UnsafeExistingLayout(run_path.clone(), error.to_string()))?;
    let run: seaf_core::LoopRun = serde_json::from_slice(&bytes)?;
    let errors = seaf_core::validate_loop_run(&run);
    if !errors.is_empty() {
        return Err(WorkspaceError::UnsafeExistingLayout(
            run_path,
            "role artifact run authority has invalid schema".to_string(),
        ));
    }
    if workspace
        .run_directory()
        .file_name()
        .and_then(|name| name.to_str())
        != Some(run.run_id.as_str())
    {
        return Err(WorkspaceError::UnsafeExistingLayout(
            run_path,
            "role artifact run authority belongs to another run directory".to_string(),
        ));
    }
    refuse_ambiguous_fixed_pointer_in_run(&run, step, attempt).map_err(|error| match error {
        WorkspaceError::UnsafeExistingLayout(_, reason) => WorkspaceError::UnsafeExistingLayout(
            workspace.run_directory().join(ARTIFACTS_DIR),
            reason,
        ),
        other => other,
    })
}

pub(crate) fn refuse_ambiguous_fixed_pointer_in_run(
    run: &seaf_core::LoopRun,
    step: LoopStepName,
    attempt: u32,
) -> Result<(), WorkspaceError> {
    if attempt < 2 {
        return Ok(());
    }
    let selected = run
        .steps
        .iter()
        .find(|record| record.name == step)
        .and_then(|record| record.artifact_path.as_deref());
    if selected.is_some_and(|path| {
        Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| artifact_attempt_from_file_name(&step_file_stem(step), name))
            == Some(1)
    }) {
        return Err(WorkspaceError::UnsafeExistingLayout(
            Path::new(ARTIFACTS_DIR).to_path_buf(),
            "ambiguous fixed role artifact pointer cannot authorize indexed publication"
                .to_string(),
        ));
    }
    Ok(())
}

pub fn next_step_attempt(
    workspace: &LoopWorkspace,
    step: LoopStepName,
) -> Result<u32, WorkspaceError> {
    latest_step_attempt(workspace, step)?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| {
            WorkspaceError::UnsafeExistingLayout(
                workspace.run_directory().join(PROMPTS_DIR),
                format!(
                    "prompt attempt sequence is exhausted for {}; start a new run",
                    step_file_stem(step)
                ),
            )
        })
}

pub fn latest_step_attempt(
    workspace: &LoopWorkspace,
    step: LoopStepName,
) -> Result<Option<u32>, WorkspaceError> {
    let prompts_dir = workspace.run_directory().join(PROMPTS_DIR);
    let stem = step_file_stem(step);
    let canonical_name = request_file_name(stem.clone(), 1);
    let mut attempts = BTreeSet::new();
    if !prompts_dir.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(&prompts_dir)? {
        let entry = entry?;
        let file_name = entry.file_name().into_string().map_err(|_| {
            WorkspaceError::UnsafeExistingLayout(
                entry.path(),
                "prompt attempt file name is not UTF-8".to_string(),
            )
        })?;
        let attempt = if file_name == canonical_name {
            Some(1)
        } else {
            let parsed = attempt_from_request_file_name(&stem, &file_name);
            if parsed.is_none()
                && file_name.starts_with(&stem)
                && file_name.contains(".attempt-")
                && file_name.ends_with(".prompt.md")
            {
                return Err(WorkspaceError::UnsafeExistingLayout(
                    entry.path(),
                    "prompt attempt file name is malformed, non-canonical, or exhausted"
                        .to_string(),
                ));
            }
            parsed
        };
        if let Some(attempt) = attempt {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(WorkspaceError::UnsafeExistingLayout(
                    entry.path(),
                    "prompt attempt is not a real regular file or is a symlink".to_string(),
                ));
            }
            if !attempts.insert(attempt) {
                return Err(WorkspaceError::UnsafeExistingLayout(
                    entry.path(),
                    "prompt attempt identity is duplicated or ambiguous".to_string(),
                ));
            }
        }
    }
    let mut expected = 1_u32;
    for attempt in &attempts {
        if *attempt != expected {
            return Err(WorkspaceError::UnsafeExistingLayout(
                prompts_dir.clone(),
                "prompt attempt sequence contains a skipped attempt".to_string(),
            ));
        }
        expected = attempt.checked_add(1).ok_or_else(|| {
            WorkspaceError::UnsafeExistingLayout(
                prompts_dir.clone(),
                "prompt attempt sequence is exhausted; start a new run".to_string(),
            )
        })?;
    }
    Ok(attempts.last().copied())
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
    let attempt = attempt.parse().ok()?;
    (attempt > 1 && request_file_name(stem.to_string(), attempt) == file_name).then_some(attempt)
}

fn validate_role_artifact_attempt_slot(
    workspace: &LoopWorkspace,
    step: LoopStepName,
    attempt: u32,
    target_name: &str,
) -> Result<(), WorkspaceError> {
    let directory = workspace.run_directory().join(ARTIFACTS_DIR);
    let stem = step_file_stem(step);
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            WorkspaceError::UnsafeExistingLayout(
                entry.path(),
                "role artifact file name is not UTF-8".to_string(),
            )
        })?;
        let parsed = artifact_attempt_from_file_name(&stem, &name);
        if parsed.is_none() && recognizable_role_attempt_name(&stem, &name) {
            return Err(WorkspaceError::UnsafeExistingLayout(
                entry.path(),
                "role artifact attempt file name is malformed, non-canonical, or exhausted"
                    .to_string(),
            ));
        }
        if parsed == Some(attempt) {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(WorkspaceError::UnsafeExistingLayout(
                    entry.path(),
                    "role artifact attempt collision is not a regular file; target is not a real regular file or is a symlink"
                        .to_string(),
                ));
            }
            if name != target_name {
                return Err(WorkspaceError::UnsafeExistingLayout(
                    entry.path(),
                    format!(
                        "role artifact attempt {attempt} already exists with a different extension"
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn artifact_attempt_from_file_name(stem: &str, file_name: &str) -> Option<u32> {
    let suffix = file_name.strip_prefix(stem)?;
    if let Some(extension) = suffix.strip_prefix('.') {
        if !extension.contains('.') && valid_extension(extension) {
            return Some(1);
        }
    }
    let suffix = suffix.strip_prefix(".attempt-")?;
    let (attempt, extension) = suffix.split_once('.')?;
    let attempt = attempt.parse::<u32>().ok()?;
    (attempt >= 2
        && format!("{attempt:03}") == suffix.split_once('.')?.0
        && !extension.contains('.')
        && valid_extension(extension))
    .then_some(attempt)
}

fn recognizable_role_attempt_name(stem: &str, file_name: &str) -> bool {
    file_name
        .strip_prefix(stem)
        .and_then(|suffix| suffix.strip_prefix(".attempt-"))
        .and_then(|suffix| suffix.split_once('.'))
        .is_some_and(|(_, extension)| !extension.contains('.') && valid_extension(extension))
}

fn valid_extension(extension: &str) -> bool {
    !extension.is_empty()
        && extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, LoopWorkspace) {
        let temp = tempfile::tempdir().expect("temp dir");
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "artifact-tests").expect("workspace");
        (temp, workspace)
    }

    #[test]
    fn exact_attempt_retry_is_idempotent_because_crash_recovery_must_not_duplicate_history() {
        let (_temp, workspace) = workspace();
        write_step_request(&workspace, LoopStepName::Research, 1, "request").expect("request");
        let artifact = ArtifactContent::new("json", b"same bytes");

        let first = write_step_artifact(&workspace, LoopStepName::Research, 1, &artifact)
            .expect("first publish");
        let second = write_step_artifact(&workspace, LoopStepName::Research, 1, &artifact)
            .expect("exact retry");

        assert_eq!(first, second);
        assert_eq!(
            fs::read(workspace.run_directory().join(first)).expect("artifact"),
            b"same bytes"
        );
    }

    #[test]
    fn different_bytes_collision_fails_without_replacing_attempt_history() {
        let (_temp, workspace) = workspace();
        write_step_request(&workspace, LoopStepName::Research, 1, "request").expect("request");
        write_step_artifact(
            &workspace,
            LoopStepName::Research,
            1,
            &ArtifactContent::new("json", b"original"),
        )
        .expect("first publish");

        let error = write_step_artifact(
            &workspace,
            LoopStepName::Research,
            1,
            &ArtifactContent::new("json", b"replacement"),
        )
        .expect_err("collision");

        assert!(error.to_string().contains("different bytes"));
        assert_eq!(
            fs::read(workspace.run_directory().join("artifacts/01-research.json"))
                .expect("preserved artifact"),
            b"original"
        );

        let extension_collision = write_step_artifact(
            &workspace,
            LoopStepName::Research,
            1,
            &ArtifactContent::new("yaml", b"original"),
        )
        .expect_err("one attempt cannot publish a second extension");
        assert!(extension_collision.to_string().contains("attempt"));
        assert!(!workspace
            .run_directory()
            .join("artifacts/01-research.yaml")
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_or_directory_collision_fails_without_following_or_truncating() {
        use std::os::unix::fs::symlink;

        for directory in [false, true] {
            let (_temp, workspace) = workspace();
            write_step_request(&workspace, LoopStepName::Research, 1, "request").expect("request");
            let path = workspace.run_directory().join("artifacts/01-research.json");
            if directory {
                fs::create_dir(&path).expect("collision directory");
            } else {
                let outside = workspace.run_directory().join("outside");
                fs::write(&outside, b"outside").expect("outside");
                symlink(&outside, &path).expect("collision symlink");
            }

            let error = write_step_artifact(
                &workspace,
                LoopStepName::Research,
                1,
                &ArtifactContent::new("json", b"artifact"),
            )
            .expect_err("unsafe collision");

            assert!(error.to_string().contains("not a real regular file"));
            if !directory {
                assert_eq!(
                    fs::read(workspace.run_directory().join("outside")).unwrap(),
                    b"outside"
                );
            }
        }
    }

    #[test]
    fn skipped_or_exhausted_attempt_fails_before_publishing_artifact_bytes() {
        let (_temp, workspace) = workspace();
        write_step_request(&workspace, LoopStepName::Research, 1, "request").expect("request");

        for attempt in [0, 2, u32::MAX] {
            let error = write_step_artifact(
                &workspace,
                LoopStepName::Research,
                attempt,
                &ArtifactContent::new("json", b"must not publish"),
            )
            .expect_err("attempt must match durable prompt authority");
            assert!(error.to_string().contains("attempt"));
        }
        assert_eq!(
            fs::read_dir(workspace.run_directory().join(ARTIFACTS_DIR))
                .expect("artifacts")
                .count(),
            0
        );
    }

    #[test]
    fn maximum_canonical_prompt_attempt_fails_boundedly_instead_of_expanding_the_range() {
        let (_temp, workspace) = workspace();
        fs::write(
            workspace
                .run_directory()
                .join("prompts/01-research.attempt-4294967295.prompt.md"),
            b"exhausted",
        )
        .expect("maximum prompt attempt");

        let error = latest_step_attempt(&workspace, LoopStepName::Research)
            .expect_err("missing attempt prefix and exhaustion must fail");

        assert!(error.to_string().contains("skipped") || error.to_string().contains("exhausted"));
    }

    #[test]
    fn indexed_publication_refuses_ambiguous_fixed_pointer_without_mutating_history() {
        use crate::state::{create_run, save_run, NewLoopRun};
        use seaf_core::LoopInputDigests;

        let (_temp, workspace) = workspace();
        write_step_request(&workspace, LoopStepName::Research, 1, "attempt one").unwrap();
        write_step_request(&workspace, LoopStepName::Research, 2, "attempt two").unwrap();
        let fixed = ArtifactContent::new("json", b"fixed history");
        fs::write(
            workspace.run_directory().join("artifacts/01-research.json"),
            fixed.bytes(),
        )
        .unwrap();
        let mut run = create_run(NewLoopRun {
            run_id: "artifact-tests".to_string(),
            ticket_id: "ticket".to_string(),
            goal_id: "goal".to_string(),
            provider: "provider".to_string(),
            model: "model".to_string(),
            input_digests: LoopInputDigests {
                ticket: "0".repeat(64),
                policy: "1".repeat(64),
                config: "2".repeat(64),
                repository: "3".repeat(64),
                eval_config: None,
            },
        });
        run.steps[0].artifact_path = Some("artifacts/01-research.json".to_string());
        run.steps[0].artifact_digest = Some(fixed.digest());
        save_run(&workspace, &run).unwrap();
        let before = snapshot(&workspace);

        let error = write_step_artifact(
            &workspace,
            LoopStepName::Research,
            2,
            &ArtifactContent::new("json", b"attempt two"),
        )
        .expect_err("ambiguous fixed pointer must block indexed publication");

        assert!(error.to_string().contains("ambiguous fixed"), "{error}");
        assert_eq!(snapshot(&workspace), before);

        run.steps[0].artifact_path = None;
        run.steps[0].artifact_digest = None;
        crate::state::write_raw_canonical_run_fixture(&workspace.run_file(), &run).unwrap();
        write_step_request(&workspace, LoopStepName::Research, 3, "attempt three").unwrap();
        let before_attempt_three = snapshot(&workspace);

        let path = write_step_artifact(
            &workspace,
            LoopStepName::Research,
            3,
            &ArtifactContent::new("json", b"attempt three"),
        )
        .expect("optional prior artifacts do not make prompt attempt history ambiguous");

        assert_eq!(path, "artifacts/01-research.attempt-003.json");
        assert_ne!(snapshot(&workspace), before_attempt_three);
    }

    fn snapshot(workspace: &LoopWorkspace) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        for directory in ["", PROMPTS_DIR, ARTIFACTS_DIR, RESPONSES_DIR] {
            let root = workspace.run_directory().join(directory);
            if !root.exists() {
                continue;
            }
            for entry in fs::read_dir(root).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_file() {
                    files.push((
                        entry.path().display().to_string(),
                        fs::read(entry.path()).unwrap(),
                    ));
                }
            }
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }
}
