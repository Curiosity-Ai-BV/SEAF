#!/usr/bin/env bash

set -euo pipefail

readonly VERSION="0.1.0"

# These limits are deliberately above the current package set while remaining
# small enough to stop accidental build outputs, large fixtures, and compressed
# payload expansion before they become distribution inputs.
readonly MAX_PACKAGE_FILE_BYTES=$((2 * 1024 * 1024))
readonly MAX_COMPRESSED_ARCHIVE_BYTES=$((8 * 1024 * 1024))
readonly MAX_UNPACKED_ARCHIVE_BYTES=$((16 * 1024 * 1024))
readonly MAX_TOTAL_UNPACKED_BYTES=$((32 * 1024 * 1024))
readonly MAX_TAR_STREAM_BYTES=$((24 * 1024 * 1024))

mode="${1:-full}"
case "$mode" in
  full | --guards-only) ;;
  *)
    echo "Usage: $0 [--guards-only]" >&2
    exit 2
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

run_git() {
  GIT_CONFIG_NOSYSTEM=1 \
    GIT_CONFIG_GLOBAL=/dev/null \
    git \
    -c core.hooksPath=/dev/null \
    -c core.fsmonitor=false \
    "$@"
}

before_status="$(run_git -C "$repo_root" status --porcelain=v1 --untracked-files=all)"
temp_root=""

fail() {
  echo "Package readiness failed: $*" >&2
  exit 1
}

cleanup() {
  local exit_code=$?
  local after_status
  after_status="$(run_git -C "$repo_root" status --porcelain=v1 --untracked-files=all)"

  if [[ -n "$temp_root" ]]; then
    rm -rf "$temp_root"
  fi

  if [[ "$before_status" != "$after_status" ]]; then
    echo "Package readiness changed the source repository:" >&2
    diff \
      <(printf '%s\n' "$before_status") \
      <(printf '%s\n' "$after_status") >&2 || true
    exit_code=1
  fi

  exit "$exit_code"
}

trap cleanup EXIT

file_size_bytes() {
  local path="$1"
  local size

  if size="$(stat -f '%z' "$path" 2>/dev/null)"; then
    printf '%s\n' "$size"
  else
    stat -c '%s' "$path"
  fi
}

validate_external_temp_root() {
  local repository="$1"
  local candidate="$2"
  local canonical_repository
  local canonical_candidate

  canonical_repository="$(cd "$repository" && pwd -P)" || return 1
  canonical_candidate="$(cd "$candidate" && pwd -P)" || return 1

  case "$canonical_candidate/" in
    "$canonical_repository/"*)
      echo "temporary root is inside the source repository: $canonical_candidate" >&2
      return 1
      ;;
  esac
}

validate_directory_within_temp_root() {
  local label="$1"
  local candidate="$2"
  local canonical_root
  local canonical_candidate

  if [[ -L "$candidate" || ! -d "$candidate" ]]; then
    echo "$label must be a regular directory inside the temporary root: $candidate" >&2
    return 1
  fi
  canonical_root="$(cd "$temp_root" && pwd -P)" || return 1
  canonical_candidate="$(cd "$candidate" && pwd -P)" || return 1
  case "$canonical_candidate/" in
    "$canonical_root/"*) ;;
    *)
      echo "$label is outside the temporary root: $canonical_candidate" >&2
      return 1
      ;;
  esac
}

validate_cargo_config_free_ancestry() {
  local candidate="$1"
  local current
  local parent
  local config

  current="$(cd "$candidate" && pwd -P)" || return 1
  while :; do
    for config in "$current/.cargo/config.toml" "$current/.cargo/config"; do
      if [[ -e "$config" || -L "$config" ]]; then
        echo "Cargo config is present in Cargo working-directory ancestry: $config" >&2
        return 1
      fi
    done
    parent="$(dirname "$current")"
    [[ "$parent" != "$current" ]] || break
    current="$parent"
  done
}

validate_isolated_cargo_home() {
  local config

  validate_directory_within_temp_root "isolated Cargo home" "$cargo_home" || return 1
  for config in "$cargo_home/config.toml" "$cargo_home/config"; do
    if [[ -e "$config" || -L "$config" ]]; then
      echo "isolated Cargo home contains configuration: $config" >&2
      return 1
    fi
  done
}

run_isolated_cargo_from() {
  local working_directory="$1"
  local target_directory="$2"
  shift 2

  validate_directory_within_temp_root "Cargo working directory" "$working_directory" ||
    return 1
  validate_directory_within_temp_root "Cargo target directory" "$target_directory" ||
    return 1
  validate_cargo_config_free_ancestry "$working_directory" || return 1
  validate_isolated_cargo_home || return 1

  (
    cd "$working_directory"
    # Wrapper variables name executable programs and are cleared so callers
    # cannot run code before rustc. Target/linker/toolchain variables remain
    # ambient for supported-platform toolchains; this gate is configuration-
    # and wrapper-isolated, not a fully hermetic compiler environment.
    env \
      -u RUSTC_WRAPPER \
      -u RUSTC_WORKSPACE_WRAPPER \
      -u CARGO_BUILD_RUSTC_WRAPPER \
      -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER \
      CARGO_HOME="$cargo_home" \
      CARGO_TARGET_DIR="$target_directory" \
      cargo "$@"
  )
}

run_isolated_cargo() {
  local target_directory="$1"
  shift
  run_isolated_cargo_from "$cargo_cwd" "$target_directory" "$@"
}

run_offline_cargo() {
  local target_directory="$1"
  shift
  (
    export CARGO_NET_OFFLINE=true
    run_isolated_cargo "$target_directory" "$@"
  )
}

run_package_cargo() {
  run_offline_cargo "$package_target" "$@"
}

validate_known_regular_input() {
  local path="$1"
  local label="$2"
  local max_bytes="$3"
  local size

  if [[ -L "$path" || ! -f "$path" ]]; then
    echo "$label must be a regular non-symlink file: $path" >&2
    return 1
  fi

  size="$(file_size_bytes "$path")" || return 1
  if ((size > max_bytes)); then
    echo "$label exceeds the per-file byte budget: $size > $max_bytes" >&2
    return 1
  fi
}

validate_package_source_tree() {
  local repository="$1"
  local package_relative="$2"
  local max_bytes="$3"
  local package_directory="$repository/$package_relative"
  local entry
  local relative
  local size

  if [[ -L "$package_directory" || ! -d "$package_directory" ]]; then
    echo "package root must be a regular directory: $package_relative" >&2
    return 1
  fi

  while IFS= read -r -d '' entry; do
    relative="${entry#"$repository/"}"
    case "$relative" in
      *$'\n'* | *$'\r'*)
        echo "package input uses an unsupported newline-bearing path: $relative" >&2
        return 1
        ;;
    esac

    if [[ -L "$entry" ]]; then
      echo "package input is a symlink: $relative" >&2
      return 1
    fi

    if [[ -d "$entry" ]]; then
      continue
    fi

    if [[ ! -f "$entry" ]]; then
      echo "package input is not a regular file: $relative" >&2
      return 1
    fi

    if ! run_git -C "$repository" ls-files --error-unmatch -- \
      ":(literal)$relative" >/dev/null 2>&1; then
      echo "package input is not tracked by Git: $relative" >&2
      return 1
    fi

    size="$(file_size_bytes "$entry")" || return 1
    if ((size > max_bytes)); then
      echo "package input exceeds the per-file byte budget: $relative ($size > $max_bytes)" >&2
      return 1
    fi
  done < <(find -P "$package_directory" -mindepth 1 -print0)
}

copy_known_regular_input() {
  local repository="$1"
  local destination="$2"
  local relative="$3"
  local source="$repository/$relative"
  local target="$destination/$relative"

  case "$relative" in
    *$'\n'* | *$'\r'*)
      echo "workspace input uses an unsupported newline-bearing path: $relative" >&2
      return 1
      ;;
  esac
  validate_known_regular_input "$source" "$relative" "$MAX_PACKAGE_FILE_BYTES" || return 1
  mkdir -p "$(dirname "$target")"
  cp -p "$source" "$target"
  validate_known_regular_input "$target" "$relative copy" "$MAX_PACKAGE_FILE_BYTES"
}

materialize_known_workspace() {
  local repository="$1"
  local destination="$2"
  shift 2
  local member
  local tracked

  for root_input in Cargo.toml Cargo.lock README.md LICENSE; do
    copy_known_regular_input "$repository" "$destination" "$root_input" || return 1
  done

  for member in "$@"; do
    while IFS= read -r -d '' tracked; do
      copy_known_regular_input "$repository" "$destination" "$tracked" || return 1
    done < <(run_git -C "$repository" ls-files -z -- "$member")
    [[ -f "$destination/$member/Cargo.toml" && ! -L "$destination/$member/Cargo.toml" ]] || {
      echo "materialized workspace member is missing Cargo.toml: $member" >&2
      return 1
    }
  done
}

build_expected_inventory() {
  local package_relative="$1"
  local destination="$2"
  local unsorted="$destination.unsorted"
  local tracked
  local relative

  : >"$unsorted"
  while IFS= read -r -d '' tracked; do
    relative="${tracked#"$package_relative/"}"
    if [[ "$relative" == "Cargo.toml" ]]; then
      printf '%s\n' Cargo.toml Cargo.toml.orig >>"$unsorted"
    else
      printf '%s\n' "$relative" >>"$unsorted"
    fi
  done < <(run_git -C "$repo_root" ls-files -z -- "$package_relative")

  printf '%s\n' Cargo.lock README.md >>"$unsorted"
  LC_ALL=C sort "$unsorted" >"$destination"
  rm -f "$unsorted"

  if [[ -n "$(uniq -d "$destination")" ]]; then
    echo "expected archive inventory contains conflicting package paths" >&2
    return 1
  fi
}

expect_guard_rejection() {
  local label="$1"
  local expected="$2"
  shift 2
  local output="$temp_root/self-test-$label.log"

  if "$@" >"$output" 2>&1; then
    fail "negative guard '$label' unexpectedly passed"
  fi
  grep -Fq "$expected" "$output" || {
    cat "$output" >&2
    fail "negative guard '$label' failed for the wrong reason"
  }
}

run_guard_self_tests() {
  local self_test_root="$temp_root/guard-self-tests"
  local fixture_repo="$self_test_root/source-repo"
  local fixture_package="$fixture_repo/crates/example"
  local malicious_root="$self_test_root/malicious-git"
  local malicious_template="$malicious_root/template"
  local malicious_hooks="$malicious_root/hooks"
  local malicious_config="$malicious_root/global.gitconfig"
  local malicious_marker="$malicious_root/hook-ran"
  local malicious_fsmonitor_marker="$malicious_root/fsmonitor-ran"
  local sanitized_repo="$self_test_root/sanitized-repo"
  local materialized_repo="$self_test_root/materialized-repo"
  local local_temp="$fixture_repo/repo-local-tmp"
  local malicious_cargo_parent="$self_test_root/malicious-cargo-parent"
  local malicious_cargo_cwd="$malicious_cargo_parent/caller"
  local malicious_cargo_project="$malicious_cargo_parent/project"
  local malicious_cargo_config="$malicious_cargo_parent/.cargo/config.toml"
  local malicious_rustc_wrapper="$malicious_cargo_parent/rustc-wrapper"
  local malicious_rustc_marker="$malicious_cargo_parent/rustc-wrapper-ran"
  local safe_cargo_cwd="$self_test_root/safe-cargo-cwd"
  local self_test_cargo_target="$self_test_root/cargo-target"

  echo "==> Exercise package-boundary negative guards"
  mkdir -p "$fixture_package/src"
  run_git -C "$fixture_repo" init -q --template=
  printf '%s\n' '[package]' 'name = "example"' 'version = "0.1.0"' >"$fixture_package/Cargo.toml"
  printf '%s\n' 'pub fn fixture() {}' >"$fixture_package/src/lib.rs"
  run_git -C "$fixture_repo" add -- crates/example/Cargo.toml crates/example/src/lib.rs
  validate_package_source_tree "$fixture_repo" crates/example "$MAX_PACKAGE_FILE_BYTES" ||
    fail "clean package-source self-test fixture was rejected"

  printf '%s\n' harmless >"$fixture_package/notes.txt"
  expect_guard_rejection \
    untracked \
    "not tracked by Git" \
    validate_package_source_tree \
    "$fixture_repo" \
    crates/example \
    "$MAX_PACKAGE_FILE_BYTES"
  rm -f "$fixture_package/notes.txt"

  ln -s lib.rs "$fixture_package/src/link.rs"
  run_git -C "$fixture_repo" add -- crates/example/src/link.rs
  expect_guard_rejection \
    symlink \
    "is a symlink" \
    validate_package_source_tree \
    "$fixture_repo" \
    crates/example \
    "$MAX_PACKAGE_FILE_BYTES"
  run_git -C "$fixture_repo" rm -q --cached -- crates/example/src/link.rs
  rm -f "$fixture_package/src/link.rs"

  dd if=/dev/zero \
    of="$fixture_package/oversized.bin" \
    bs=1 \
    count=0 \
    seek=$((MAX_PACKAGE_FILE_BYTES + 1)) 2>/dev/null
  run_git -C "$fixture_repo" add -- crates/example/oversized.bin
  expect_guard_rejection \
    oversized \
    "exceeds the per-file byte budget" \
    validate_package_source_tree \
    "$fixture_repo" \
    crates/example \
    "$MAX_PACKAGE_FILE_BYTES"
  run_git -C "$fixture_repo" rm -q --cached -- crates/example/oversized.bin
  rm -f "$fixture_package/oversized.bin"

  printf '%s\n' '[workspace]' >"$fixture_repo/Cargo.toml"
  printf '%s\n' '# lock fixture' >"$fixture_repo/Cargo.lock"
  printf '%s\n' '# fixture' >"$fixture_repo/README.md"
  printf '%s\n' 'fixture license' >"$fixture_repo/LICENSE"
  dd if=/dev/zero \
    of="$fixture_repo/untracked-root-blob.bin" \
    bs=1 \
    count=0 \
    seek=$((MAX_PACKAGE_FILE_BYTES + 1)) 2>/dev/null
  mkdir -p "$materialized_repo"
  materialize_known_workspace \
    "$fixture_repo" \
    "$materialized_repo" \
    crates/example ||
    fail "known-input workspace materialization rejected its clean fixture"
  [[ ! -e "$materialized_repo/untracked-root-blob.bin" ]] ||
    fail "workspace materialization copied an untracked root payload"

  mkdir -p "$local_temp"
  expect_guard_rejection \
    repo-local-tmp \
    "inside the source repository" \
    validate_external_temp_root \
    "$fixture_repo" \
    "$local_temp"

  mkdir -p "$malicious_template" "$malicious_hooks" "$sanitized_repo"
  printf '%s\n' injected >"$malicious_template/INJECTED"
  printf '#!/usr/bin/env bash\ntouch %q\n' "$malicious_marker" >"$malicious_hooks/pre-commit"
  printf '#!/usr/bin/env bash\ntouch %q\nprintf "{}"\n' \
    "$malicious_fsmonitor_marker" >"$malicious_root/fsmonitor"
  chmod +x "$malicious_hooks/pre-commit" "$malicious_root/fsmonitor"
  printf '[init]\n\ttemplateDir = %s\n[core]\n\thooksPath = %s\n\tfsmonitor = %s\n' \
    "$malicious_template" \
    "$malicious_hooks" \
    "$malicious_root/fsmonitor" >"$malicious_config"

  GIT_CONFIG_GLOBAL="$malicious_config" \
    run_git -C "$sanitized_repo" init -q --template=
  [[ ! -e "$sanitized_repo/INJECTED" ]] ||
    fail "sanitized Git init consumed a global template"
  printf '%s\n' clean >"$sanitized_repo/tracked.txt"
  GIT_CONFIG_GLOBAL="$malicious_config" \
    run_git -C "$sanitized_repo" add -- tracked.txt
  GIT_CONFIG_GLOBAL="$malicious_config" \
    run_git \
    -C "$sanitized_repo" \
    -c user.name="SEAF Package Guard" \
    -c user.email="package-guard@seaf.invalid" \
    commit -q -m "Exercise sanitized Git"
  GIT_CONFIG_GLOBAL="$malicious_config" \
    run_git -C "$sanitized_repo" status --porcelain=v1 >/dev/null
  [[ ! -e "$malicious_marker" && ! -e "$malicious_fsmonitor_marker" ]] ||
    fail "sanitized Git operation executed a global hook or fsmonitor"

  mkdir -p \
    "$malicious_cargo_cwd" \
    "$malicious_cargo_project/src" \
    "$(dirname "$malicious_cargo_config")" \
    "$safe_cargo_cwd" \
    "$self_test_cargo_target"
  printf '%s\n' \
    '[package]' \
    'name = "cargo-boundary-fixture"' \
    'version = "0.1.0"' \
    'edition = "2021"' >"$malicious_cargo_project/Cargo.toml"
  printf '%s\n' 'pub fn fixture() {}' >"$malicious_cargo_project/src/lib.rs"
  printf '#!/usr/bin/env bash\ntouch %q\nexec "$@"\n' \
    "$malicious_rustc_marker" >"$malicious_rustc_wrapper"
  chmod +x "$malicious_rustc_wrapper"
  printf '[build]\nrustc-wrapper = "%s"\n' \
    "$malicious_rustc_wrapper" >"$malicious_cargo_config"

  expect_guard_rejection \
    ancestor-cargo-config \
    "Cargo config is present in Cargo working-directory ancestry" \
    run_isolated_cargo_from \
    "$malicious_cargo_cwd" \
    "$self_test_cargo_target" \
    check \
    --offline \
    --manifest-path "$malicious_cargo_project/Cargo.toml"
  [[ ! -e "$malicious_rustc_marker" ]] ||
    fail "ancestor Cargo config executed a rustc wrapper before rejection"

  RUSTC_WRAPPER="$malicious_rustc_wrapper" \
    RUSTC_WORKSPACE_WRAPPER="$malicious_rustc_wrapper" \
    run_isolated_cargo_from \
    "$safe_cargo_cwd" \
    "$self_test_cargo_target" \
    check \
    --offline \
    --manifest-path "$malicious_cargo_project/Cargo.toml"
  [[ ! -e "$malicious_rustc_marker" ]] ||
    fail "ambient Cargo wrapper variables executed inside the isolated gate"
}

temp_root="$(mktemp -d "${TMPDIR:-/tmp}/seaf-package-readiness.XXXXXX")"
temp_root="$(cd "$temp_root" && pwd -P)"
validate_external_temp_root "$repo_root" "$temp_root" ||
  fail "temporary package root must be outside the source repository"

cargo_home="$temp_root/cargo-home"
cargo_cwd="$temp_root/cargo-cwd"
package_target="$temp_root/package-target"
install_target="$temp_root/install-target"
extract_root="$temp_root/extracted"
install_root="$temp_root/install"
project_root="$temp_root/project"
package_workspace="$temp_root/package-workspace"
expected_inventory_root="$temp_root/expected-inventory"
mkdir -p \
  "$cargo_home" \
  "$cargo_cwd" \
  "$package_target" \
  "$install_target" \
  "$extract_root" \
  "$install_root" \
  "$project_root" \
  "$package_workspace" \
  "$expected_inventory_root"

run_guard_self_tests

validate_known_regular_input "$repo_root/README.md" README "$MAX_PACKAGE_FILE_BYTES" ||
  fail "README package input is unsafe"
validate_known_regular_input "$repo_root/LICENSE" LICENSE "$MAX_PACKAGE_FILE_BYTES" ||
  fail "LICENSE package input is unsafe"
validate_known_regular_input "$repo_root/Cargo.toml" Cargo.toml "$MAX_PACKAGE_FILE_BYTES" ||
  fail "workspace manifest package input is unsafe"
validate_known_regular_input "$repo_root/Cargo.lock" Cargo.lock "$MAX_PACKAGE_FILE_BYTES" ||
  fail "workspace lock package input is unsafe"

for package in seaf-core seaf-models seaf-loop seaf-cli; do
  package_relative="crates/$package"
  validate_package_source_tree \
    "$repo_root" \
    "$package_relative" \
    "$MAX_PACKAGE_FILE_BYTES" ||
    fail "$package source inventory is unsafe"
  cmp -s "$repo_root/LICENSE" "$repo_root/$package_relative/LICENSE" ||
    fail "$package LICENSE differs from the repository license"
  build_expected_inventory \
    "$package_relative" \
    "$expected_inventory_root/$package.files" ||
    fail "$package expected inventory could not be built"
done

if [[ "$mode" == "--guards-only" ]]; then
  echo "Package readiness negative guards passed."
  exit 0
fi

materialize_known_workspace \
  "$repo_root" \
  "$package_workspace" \
  crates/seaf-core \
  crates/seaf-cli \
  crates/seaf-local-runtime \
  crates/seaf-models \
  crates/seaf-loop ||
  fail "could not materialize the tracked workspace package inputs"

assert_tar_stream_budget() {
  local archive="$1"
  local probe="$2"
  local pipeline_status
  local gzip_exit
  local head_exit
  local probe_size

  set +e
  set +o pipefail
  gzip -dc "$archive" | head -c $((MAX_TAR_STREAM_BYTES + 1)) >"$probe"
  pipeline_status=("${PIPESTATUS[@]}")
  gzip_exit=${pipeline_status[0]}
  head_exit=${pipeline_status[1]}
  set -o pipefail
  set -e

  ((head_exit == 0)) || fail "could not inspect archive stream: $archive"
  probe_size="$(file_size_bytes "$probe")"
  if ((probe_size > MAX_TAR_STREAM_BYTES)); then
    fail "archive tar stream exceeds $MAX_TAR_STREAM_BYTES bytes: $archive"
  fi
  ((gzip_exit == 0)) || fail "archive gzip stream is invalid: $archive"
}

validate_archive_inventory() {
  local archive="$1"
  local package_root="$2"
  local expected="$3"
  local actual="$4"
  local names="$actual.names"
  local verbose="$actual.verbose"
  local unsorted="$actual.unsorted"
  local name
  local relative
  local name_count
  local verbose_count

  tar -tzf "$archive" >"$names"
  LC_ALL=C tar -tvzf "$archive" >"$verbose"
  name_count="$(wc -l <"$names" | tr -d ' ')"
  verbose_count="$(wc -l <"$verbose" | tr -d ' ')"
  [[ "$name_count" == "$verbose_count" ]] ||
    fail "$package_root archive inventory contains an ambiguous newline-bearing name"

  if ! awk 'substr($0, 1, 1) != "-" { exit 1 }' "$verbose"; then
    fail "$package_root archive contains a non-regular tar entry"
  fi

  : >"$unsorted"
  while IFS= read -r name; do
    [[ -n "$name" ]] || fail "$package_root archive contains an empty name"
    case "$name" in
      /*) fail "$package_root archive contains an absolute name: $name" ;;
      "$package_root"/*) ;;
      *) fail "$package_root archive escapes its package root: $name" ;;
    esac

    relative="${name#"$package_root/"}"
    [[ -n "$relative" ]] || fail "$package_root archive contains its root entry"
    case "/$relative/" in
      *'/../'* | *'/./'* | *'//'*)
        fail "$package_root archive contains a traversal-shaped name: $name"
        ;;
    esac
    printf '%s\n' "$relative" >>"$unsorted"
  done <"$names"

  LC_ALL=C sort "$unsorted" >"$actual"
  rm -f "$names" "$verbose" "$unsorted"
  if [[ -n "$(uniq -d "$actual")" ]]; then
    fail "$package_root archive contains duplicate names"
  fi

  if ! cmp -s "$expected" "$actual"; then
    diff -u "$expected" "$actual" >&2 || true
    fail "$package_root archive inventory differs from tracked package inputs"
  fi
}

assert_normalized_manifest_metadata() {
  local manifest="$1"
  local package="$2"

  for expected in \
    'version = "0.1.0"' \
    'publish = false' \
    'readme = "README.md"' \
    'license = "MIT"' \
    'repository = "https://github.com/Curiosity-Ai-BV/SEAF"'; do
    grep -Fxq "$expected" "$manifest" ||
      fail "$package normalized manifest is missing: $expected"
  done

  grep -Eq '^description = "[^"].*"$' "$manifest" ||
    fail "$package normalized manifest has no description"
  if grep -Eq '^license-file[[:space:]]*=' "$manifest"; then
    fail "$package normalized manifest retained warning-producing license-file metadata"
  fi
}

assert_exact_normalized_dependency() {
  local manifest="$1"
  local dependency="$2"

  awk -v section="[dependencies.$dependency]" '
    $0 == section { in_dependency = 1; found = 1; next }
    /^\[/ { in_dependency = 0 }
    in_dependency && $0 == "version = \"=0.1.0\"" { exact_version = 1 }
    in_dependency && $0 ~ /^path[[:space:]]*=/ { path_dependency = 1 }
    END { exit !(found && exact_version && !path_dependency) }
  ' "$manifest" ||
    fail "$dependency is not an exact path-free =0.1.0 packaged dependency"
}

total_unpacked_bytes=0

measure_extracted_inventory() {
  local package_dir="$1"
  local inventory="$2"
  local package="$3"
  local relative
  local file
  local size
  local package_bytes=0

  while IFS= read -r relative; do
    file="$package_dir/$relative"
    if [[ -L "$file" || ! -f "$file" ]]; then
      fail "$package extracted a non-regular inventory entry: $relative"
    fi
    size="$(file_size_bytes "$file")"
    if ((size > MAX_PACKAGE_FILE_BYTES)); then
      fail "$package extracted file exceeds $MAX_PACKAGE_FILE_BYTES bytes: $relative"
    fi
    package_bytes=$((package_bytes + size))
  done <"$inventory"

  if ((package_bytes > MAX_UNPACKED_ARCHIVE_BYTES)); then
    fail "$package exceeds the $MAX_UNPACKED_ARCHIVE_BYTES byte unpacked budget"
  fi
  total_unpacked_bytes=$((total_unpacked_bytes + package_bytes))
  if ((total_unpacked_bytes > MAX_TOTAL_UNPACKED_BYTES)); then
    fail "distribution packages exceed the $MAX_TOTAL_UNPACKED_BYTES byte aggregate budget"
  fi
}

package_and_extract() {
  local package="$1"
  shift
  local package_root="$package-$VERSION"
  local archive="$package_target/package/$package_root.crate"
  local actual_inventory="$temp_root/$package.actual-files"
  local expected_inventory="$expected_inventory_root/$package.files"
  local archive_size

  echo "==> Package and verify $package"
  run_package_cargo package \
    --locked \
    --offline \
    --allow-dirty \
    --manifest-path "$package_workspace/Cargo.toml" \
    -p "$package" \
    "$@"

  [[ -f "$archive" && ! -L "$archive" ]] ||
    fail "$package did not produce a regular archive"
  archive_size="$(file_size_bytes "$archive")"
  if ((archive_size > MAX_COMPRESSED_ARCHIVE_BYTES)); then
    fail "$package archive exceeds the $MAX_COMPRESSED_ARCHIVE_BYTES byte compressed budget"
  fi
  assert_tar_stream_budget "$archive" "$temp_root/$package.tar-stream"
  validate_archive_inventory \
    "$archive" \
    "$package_root" \
    "$expected_inventory" \
    "$actual_inventory"

  tar -xzf "$archive" -C "$extract_root"
  [[ -d "$extract_root/$package_root" && ! -L "$extract_root/$package_root" ]] ||
    fail "$package archive did not extract to a regular expected directory"
  measure_extracted_inventory \
    "$extract_root/$package_root" \
    "$actual_inventory" \
    "$package"
  assert_normalized_manifest_metadata "$extract_root/$package_root/Cargo.toml" "$package"

  packaged_archive="$archive"
  packaged_inventory="$actual_inventory"
  packaged_root="$package_root"
  packaged_dir="$extract_root/$package_root"
}

assert_archive_contains() {
  local inventory="$1"
  local package_root="$2"
  local relative_path="$3"

  grep -Fxq "$relative_path" "$inventory" ||
    fail "$package_root archive is missing $relative_path"
}

assert_exact_output() {
  local label="$1"
  local expected="$2"
  shift 2
  local stdout_file="$temp_root/$label.stdout"
  local stderr_file="$temp_root/$label.stderr"
  local expected_file="$temp_root/$label.expected"

  printf '%s\n' "$expected" >"$expected_file"
  "$@" >"$stdout_file" 2>"$stderr_file" || {
    cat "$stderr_file" >&2
    fail "$label command failed"
  }
  cmp -s "$expected_file" "$stdout_file" || {
    echo "Expected $label output:" >&2
    cat "$expected_file" >&2
    echo "Actual $label output:" >&2
    cat "$stdout_file" >&2
    fail "$label output was not exact"
  }
  [[ ! -s "$stderr_file" ]] || {
    cat "$stderr_file" >&2
    fail "$label wrote unexpected stderr"
  }
}

echo "==> Fetch locked dependencies once into an isolated Cargo home"
run_isolated_cargo \
  "$package_target" \
  fetch \
  --locked \
  --manifest-path "$package_workspace/Cargo.toml"

package_and_extract seaf-core
core_dir="$packaged_dir"
assert_archive_contains "$packaged_inventory" "$packaged_root" "src/lib.rs"
for template in \
  adaptive.yaml \
  generic-seaf.config.json \
  generic-seaf.gitignore \
  generic-seaf.policy.json \
  seaf.evals.yaml \
  seaf.policy.json; do
  assert_archive_contains "$packaged_inventory" "$packaged_root" "templates/$template"
done

package_and_extract seaf-models
models_dir="$packaged_dir"
for source_file in fake.rs lib.rs ollama.rs provider.rs; do
  assert_archive_contains "$packaged_inventory" "$packaged_root" "src/$source_file"
done

core_patch=(--config "patch.crates-io.seaf-core.path=\"$core_dir\"")
models_patch=(--config "patch.crates-io.seaf-models.path=\"$models_dir\"")

run_package_cargo generate-lockfile \
  --offline \
  --manifest-path "$package_workspace/Cargo.toml" \
  "${core_patch[@]}" \
  "${models_patch[@]}"
package_and_extract seaf-loop "${core_patch[@]}" "${models_patch[@]}"
loop_dir="$packaged_dir"
assert_exact_normalized_dependency "$loop_dir/Cargo.toml" seaf-core
assert_exact_normalized_dependency "$loop_dir/Cargo.toml" seaf-models
for source_file in lib.rs candidate_workspace.rs model_runner.rs; do
  assert_archive_contains "$packaged_inventory" "$packaged_root" "src/$source_file"
done

loop_patch=(--config "patch.crates-io.seaf-loop.path=\"$loop_dir\"")

run_package_cargo generate-lockfile \
  --offline \
  --manifest-path "$package_workspace/Cargo.toml" \
  "${core_patch[@]}" \
  "${models_patch[@]}" \
  "${loop_patch[@]}"
package_and_extract \
  seaf-cli \
  "${core_patch[@]}" \
  "${models_patch[@]}" \
  "${loop_patch[@]}"
cli_dir="$packaged_dir"
assert_exact_normalized_dependency "$cli_dir/Cargo.toml" seaf-core
assert_exact_normalized_dependency "$cli_dir/Cargo.toml" seaf-models
assert_exact_normalized_dependency "$cli_dir/Cargo.toml" seaf-loop
assert_archive_contains "$packaged_inventory" "$packaged_root" "src/main.rs"
assert_archive_contains "$packaged_inventory" "$packaged_root" "src/doctor.rs"

if grep -Fq "$repo_root" "$cli_dir/Cargo.toml" ||
  grep -Eq 'path[[:space:]]*=[[:space:]]*"\.\./seaf-' "$cli_dir/Cargo.toml"; then
  fail "normalized CLI manifest retained a source-workspace dependency path"
fi

echo "==> Install the exact extracted CLI package outside the source workspace"
run_offline_cargo \
  "$install_target" \
  install \
  --locked \
  --offline \
  --root "$install_root" \
  --path "$cli_dir" \
  "${core_patch[@]}" \
  "${models_patch[@]}" \
  "${loop_patch[@]}"

seaf_bin="$install_root/bin/seaf"
[[ -x "$seaf_bin" && ! -L "$seaf_bin" ]] || fail "installed CLI binary is unsafe or missing"
case "$seaf_bin" in
  "$repo_root"/*) fail "installed CLI binary is inside the source repository" ;;
esac

assert_exact_output version "seaf 0.1.0" "$seaf_bin" --version
assert_exact_output info "Self-Evolving Application Framework" "$seaf_bin" info

echo "==> Smoke the installed CLI in a fresh external Git project"
run_git -C "$project_root" init -q --template=
(
  cd "$project_root"
  "$seaf_bin" init --json >"$temp_root/init.json"
  run_git add --all
  run_git \
    -c user.name="SEAF Package Gate" \
    -c user.email="package-gate@seaf.invalid" \
    commit -q -m "Initialize SEAF package smoke"
  "$seaf_bin" doctor --provider fake --json >"$temp_root/doctor.json"
)

for initialized_file in \
  seaf.config.json \
  seaf.policy.json \
  seaf.evals.yaml \
  seaf.ticket.yaml \
  .seaf/.gitignore; do
  [[ -f "$project_root/$initialized_file" && ! -L "$project_root/$initialized_file" ]] ||
    fail "installed init did not create a regular $initialized_file"
done

grep -Fq '"ready": true' "$temp_root/doctor.json" || {
  cat "$temp_root/doctor.json" >&2
  fail "installed doctor did not report ready"
}

[[ -z "$(run_git -C "$project_root" status --porcelain=v1)" ]] ||
  fail "installed package smoke left its external project dirty"

echo "Package readiness passed for seaf-core, seaf-models, seaf-loop, and seaf-cli $VERSION."
