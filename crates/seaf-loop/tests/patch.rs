use seaf_loop::{parse_unified_diff, PatchParseError};

fn fixture(name: &str) -> &'static str {
    match name {
        "allowed-doc.diff" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/patches/allowed-doc.diff"
        )),
        "path-traversal.diff" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/patches/path-traversal.diff"
        )),
        "binary.diff" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/patches/binary.diff"
        )),
        _ => panic!("unknown fixture: {name}"),
    }
}

#[test]
fn patch_parser_extracts_normalized_paths_from_unified_diff() {
    let parsed = parse_unified_diff(fixture("allowed-doc.diff")).expect("parse safe patch");

    assert_eq!(parsed.changed_paths, vec!["docs/example.md"]);
    assert!(
        !parsed.contains_binary_patch,
        "text patches should remain eligible for deterministic policy checks"
    );
}

#[test]
fn patch_parser_tracks_new_and_deleted_files_without_treating_dev_null_as_repo_path() {
    let patch = r#"diff --git a/docs/new.md b/docs/new.md
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1 @@
+new
diff --git a/docs/old.md b/docs/old.md
deleted file mode 100644
index 1111111..0000000
--- a/docs/old.md
+++ /dev/null
@@ -1 +0,0 @@
-old
"#;

    let parsed = parse_unified_diff(patch).expect("parse add and delete patch");

    assert_eq!(parsed.changed_paths, vec!["docs/new.md", "docs/old.md"]);
    assert!(
        parsed
            .files
            .iter()
            .all(|file| file.paths.iter().all(|path| path != "/dev/null")),
        "/dev/null is a diff sentinel, not a mutable repository path"
    );
}

#[test]
fn patch_parser_supports_quoted_paths_from_git_headers() {
    let patch = r#"diff --git "a/docs/quoted name.md" "b/docs/quoted name.md"
index 1111111..2222222 100644
--- "a/docs/quoted name.md"
+++ "b/docs/quoted name.md"
@@ -1 +1 @@
-old
+new
"#;

    let parsed = parse_unified_diff(patch).expect("parse quoted path patch");

    assert_eq!(parsed.changed_paths, vec!["docs/quoted name.md"]);
}

#[test]
fn patch_parser_rejects_diff_git_headers_with_extra_tokens() {
    let patch = r#"diff --git a/docs/a.md b/docs/a.md extra
index 1111111..2222222 100644
--- a/docs/a.md
+++ b/docs/a.md
@@ -1 +1 @@
-old
+new
"#;

    let error = parse_unified_diff(patch).unwrap_err();

    assert_eq!(
        error,
        PatchParseError::MalformedGitHeader("a/docs/a.md b/docs/a.md extra".to_string()),
        "extra header tokens must fail closed instead of being ignored"
    );
}

#[test]
fn patch_parser_rejects_path_traversal_before_policy_or_apply() {
    let error = parse_unified_diff(fixture("path-traversal.diff")).unwrap_err();

    assert_eq!(
        error,
        PatchParseError::UnsafePath("../escape.md".to_string()),
        "path traversal must fail closed before a patch reaches git apply"
    );
}

#[test]
fn patch_parser_rejects_absolute_and_backslash_paths_before_apply() {
    for (patch, expected_path) in [
        (
            r#"diff --git a/docs/example.md b//tmp/example.md
index 1111111..2222222 100644
--- a/docs/example.md
+++ b//tmp/example.md
@@ -1 +1 @@
-old
+new
"#,
            "/tmp/example.md",
        ),
        (
            r#"diff --git a/docs/example.md b/docs\example.md
index 1111111..2222222 100644
--- a/docs/example.md
+++ b/docs\example.md
@@ -1 +1 @@
-old
+new
"#,
            "docs\\example.md",
        ),
    ] {
        let error = parse_unified_diff(patch).unwrap_err();

        assert_eq!(
            error,
            PatchParseError::UnsafePath(expected_path.to_string()),
            "unsafe paths must fail closed before git apply can interpret them"
        );
    }
}

#[test]
fn patch_parser_detects_binary_patches_for_policy_rejection() {
    let parsed = parse_unified_diff(fixture("binary.diff")).expect("parse binary patch metadata");

    assert_eq!(parsed.changed_paths, vec!["assets/logo.png"]);
    assert!(
        parsed.contains_binary_patch,
        "binary patches cannot be inspected as text and must be rejected by policy"
    );
}
