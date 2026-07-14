#!/usr/bin/env bash

set -euo pipefail

readonly VERSION="0.1.0"
readonly LINUX_TARGET="x86_64-unknown-linux-gnu"
readonly MACOS_TARGET="aarch64-apple-darwin"
readonly MAX_FILE_BYTES=$((64 * 1024 * 1024))

mode="${1:-full}"
case "$mode" in
  full | --review-regressions-only) ;;
  *)
    echo "Usage: $0 [--review-regressions-only]" >&2
    exit 2
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
build_script="$repo_root/scripts/build-release-artifact.sh"
assemble_script="$repo_root/scripts/assemble-release-assets.sh"
temp_root=""

run_git() {
  GIT_CONFIG_NOSYSTEM=1 \
    GIT_CONFIG_GLOBAL=/dev/null \
    git \
    -c core.hooksPath=/dev/null \
    -c core.fsmonitor=false \
    "$@"
}

before_status="$(run_git -C "$repo_root" status --porcelain=v1 --untracked-files=all)"

fail() {
  echo "Release artifact test failed: $*" >&2
  exit 1
}

cleanup() {
  local exit_code=$?
  local after_status

  if [[ -n "$temp_root" ]]; then
    rm -rf "$temp_root"
  fi

  after_status="$(run_git -C "$repo_root" status --porcelain=v1 --untracked-files=all)"
  if [[ "$before_status" != "$after_status" ]]; then
    echo "Release artifact test changed the source repository:" >&2
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

  if stat -f '%z' "$path" >/dev/null 2>&1; then
    stat -f '%z' "$path"
  else
    stat -c '%s' "$path"
  fi
}

file_mode() {
  local path="$1"

  if stat -f '%Lp' "$path" >/dev/null 2>&1; then
    stat -f '%Lp' "$path"
  else
    stat -c '%a' "$path"
  fi
}

sha256_file() {
  local path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  else
    shasum -a 256 "$path" | awk '{print $1}'
  fi
}

assert_exact_output() {
  local label="$1"
  local expected="$2"
  shift 2
  local actual

  actual="$("$@")" || fail "$label command failed"
  [[ "$actual" == "$expected" ]] ||
    fail "$label output was '$actual', expected '$expected'"
}

assert_fails_with() {
  local label="$1"
  local expected="$2"
  shift 2
  local output

  if output="$("$@" 2>&1)"; then
    fail "$label unexpectedly succeeded"
  fi
  [[ "$output" == *"$expected"* ]] ||
    fail "$label did not report '$expected'; output: $output"
}

require_release_scripts() {
  local missing=()

  [[ -x "$build_script" ]] || missing+=("scripts/build-release-artifact.sh")
  [[ -x "$assemble_script" ]] || missing+=("scripts/assemble-release-assets.sh")

  if ((${#missing[@]} > 0)); then
    echo "RED: missing release artifact implementation: ${missing[*]}" >&2
    [[ -e "$repo_root/.github/workflows/release-artifacts.yml" ]] ||
      echo "RED seam evidence: .github/workflows/release-artifacts.yml is absent" >&2
    [[ -e "$repo_root/docs/release-artifacts.md" ]] ||
      echo "RED seam evidence: docs/release-artifacts.md is absent" >&2
    grep -Fq './scripts/test-release-artifacts.sh' "$repo_root/.github/workflows/ci.yml" ||
      echo "RED seam evidence: ordinary CI has no release artifact test step" >&2
    fail "required build/assembly scripts are missing"
  fi
}

assert_file_contains() {
  local path="$1"
  local expected="$2"

  grep -Fq -- "$expected" "$path" ||
    fail "$(basename "$path") is missing required contract text: $expected"
}

assert_occurrences() {
  local path="$1"
  local expected="$2"
  local count="$3"
  local actual

  actual="$(grep -Fc -- "$expected" "$path" || true)"
  [[ "$actual" == "$count" ]] ||
    fail "$(basename "$path") contains '$expected' $actual times, expected $count"
}

assert_static_release_contract() {
  local workflow="$repo_root/.github/workflows/release-artifacts.yml"
  local release_docs="$repo_root/docs/release-artifacts.md"
  local run_blocks identity_run_block identity_definition_count identity_call_count
  local identity_limit_count

  [[ -f "$workflow" ]] || fail "release artifact workflow is missing"
  [[ -f "$release_docs" ]] || fail "release artifact documentation is missing"
  assert_file_contains "$repo_root/.github/workflows/ci.yml" \
    './scripts/test-release-artifacts.sh'

  assert_file_contains "$workflow" 'tags:'
  assert_file_contains "$workflow" '- "v*"'
  assert_file_contains "$workflow" 'contents: read'
  assert_occurrences "$workflow" 'runner: ubuntu-22.04' 1
  assert_occurrences "$workflow" 'runner: macos-15' 1
  assert_occurrences "$workflow" "target: $LINUX_TARGET" 1
  assert_occurrences "$workflow" "target: $MACOS_TARGET" 1
  assert_occurrences "$workflow" \
    'actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd' 2
  assert_occurrences "$workflow" 'persist-credentials: false' 2
  assert_occurrences "$workflow" \
    'dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30' 1
  assert_occurrences "$workflow" 'toolchain: stable' 1
  assert_occurrences "$workflow" \
    'actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a' 2
  assert_occurrences "$workflow" \
    'actions/download-artifact@70fc10c6e5e1ce46ad2ea6f2b72d43f7d47b13c3' 1
  assert_occurrences "$workflow" 'retention-days: 2' 2
  assert_occurrences "$workflow" 'github.run_attempt' 3
  assert_file_contains "$workflow" 'cargo build --locked --release -p seaf-cli --bin seaf'
  assert_file_contains "$workflow" 'GITHUB_REF_TYPE'
  assert_file_contains "$workflow" 'GITHUB_REF_NAME'
  assert_file_contains "$workflow" 'GITHUB_SHA'
  assert_file_contains "$workflow" 'rustc -vV'
  assert_file_contains "$workflow" './scripts/build-release-artifact.sh build'
  assert_file_contains "$workflow" './scripts/build-release-artifact.sh verify'
  assert_file_contains "$workflow" './scripts/assemble-release-assets.sh assemble'
  assert_file_contains "$workflow" './scripts/assemble-release-assets.sh verify'
  assert_file_contains "$workflow" 'merge-multiple: true'
  assert_file_contains "$workflow" 'seaf $workspace_version'
  assert_file_contains "$workflow" 'Self-Evolving Application Framework'
  assert_occurrences "$workflow" 'assert_cli_identity() {' 1
  assert_occurrences "$workflow" 'assert_cli_identity \' 4
  assert_file_contains "$workflow" 'status=$?'
  assert_file_contains "$workflow" '2>"$stderr_path"'
  assert_file_contains "$workflow" 'test "$status" -eq 0'
  assert_file_contains "$workflow" 'test ! -s "$stderr_path"'
  assert_file_contains "$workflow" 'cmp -s "$expected_path" "$stdout_path"'
  if grep -Fq 'test "$("$' "$workflow"; then
    fail "workflow identity still discards command status and stderr"
  fi
  identity_run_block="$(awk '
    /^      - name: Build, validate, and smoke the native artifact$/ {
      in_step = 1
      next
    }
    in_step && /^      - name:/ { exit }
    in_step && /^        run: \|$/ { in_run = 1; next }
    in_run { print }
  ' "$workflow")"
  identity_definition_count="$(printf '%s\n' "$identity_run_block" |
    grep -Fc 'assert_cli_identity() {' || true)"
  identity_call_count="$(printf '%s\n' "$identity_run_block" |
    grep -Fc 'assert_cli_identity \' || true)"
  identity_limit_count="$(printf '%s\n' "$identity_run_block" |
    grep -Fc 'ulimit -f "$((64 * 1024 * 1024 / 1024))"' || true)"
  [[ "$identity_definition_count" == "1" && \
    "$identity_call_count" == "4" && \
    "$identity_limit_count" == "1" ]] ||
    fail "workflow bounded identity helper and its four calls are not in the same run block"
  assert_occurrences "$workflow" 'ulimit -f "$((64 * 1024 * 1024 / 1024))"' 1
  assert_file_contains "$build_script" 'ulimit -f "$((MAX_FILE_BYTES / 1024))"'
  assert_file_contains "$build_script" "grep -q 'GNU tar'"
  assert_file_contains "$build_script" '--format=ustar'
  assert_file_contains "$build_script" '--owner=0'
  assert_file_contains "$build_script" '--group=0'
  assert_file_contains "$build_script" '--numeric-owner'
  assert_file_contains "$build_script" '--mtime=@0'
  assert_file_contains "$build_script" '--format ustar'
  assert_file_contains "$build_script" '--uid 0'
  assert_file_contains "$build_script" '--gid 0'
  assert_file_contains "$build_script" "--uname ''"
  assert_file_contains "$build_script" "--gname ''"

  for forbidden in \
    workflow_dispatch \
    'pull_request:' \
    pull_request_target \
    workflow_call \
    schedule: \
    branches: \
    'contents: write' \
    'id-token:' \
    'attestations:' \
    'environment:' \
    'secrets.' \
    'gh release' \
    'git tag' \
    'git push'; do
    if grep -Fq -- "$forbidden" "$workflow"; then
      fail "release artifact workflow contains forbidden authority: $forbidden"
    fi
  done

  run_blocks="$(awk '
    /^[[:space:]]+run: \|$/ { in_run = 1; next }
    in_run && /^[[:space:]]+- (name|uses):/ { in_run = 0 }
    in_run { print }
  ' "$workflow")"
  [[ "$run_blocks" != *'${{'* ]] ||
    fail "GitHub context interpolation appears inside workflow shell code"

  assert_file_contains "$release_docs" "seaf-v${VERSION}-${LINUX_TARGET}.tar.gz"
  assert_file_contains "$release_docs" "seaf-v${VERSION}-${MACOS_TARGET}.tar.gz"
  assert_file_contains "$release_docs" 'SHA256SUMS'
  assert_file_contains "$release_docs" 'create or push a tag'
  assert_file_contains "$release_docs" 'M2-05'
  assert_file_contains "$repo_root/README.md" 'docs/release-artifacts.md'
  assert_file_contains "$repo_root/docs/supported-platforms.md" "$LINUX_TARGET"
  assert_file_contains "$repo_root/docs/supported-platforms.md" "$MACOS_TARGET"
  assert_file_contains "$repo_root/docs/production-use-implementation-plan.md" \
    'Status: accepted on 2026-07-14. Dependencies: M2-03 (accepted). M2-05 is'
  assert_file_contains "$repo_root/docs/production-readiness-roadmap.md" \
    'Milestone 1 and M2-01 through M2-04 are complete.'
  assert_file_contains "$repo_root/.seaf/loops/current/progress.md" \
    '[x] M2-04: Release artifact workflow.'
  assert_file_contains "$repo_root/.seaf/loops/current/log.md" \
    '2026-07-14 accepted | M2-04 release artifact workflow'
  assert_file_contains "$repo_root/.seaf/loops/current/contract.md" \
    'Status: awaiting-explicit-user-authorization on 2026-07-14.'
}

make_bad_archive() {
  local kind="$1"
  local target="$2"
  local archive="$3"
  local root="seaf-v${VERSION}-${target}"
  local fixture="$temp_root/bad-$kind"
  local tar_path="$fixture/payload.tar"

  mkdir -p "$fixture/stage/$root"
  cp "$repo_root/CHANGELOG.md" "$fixture/stage/$root/CHANGELOG.md"
  cp "$repo_root/LICENSE" "$fixture/stage/$root/LICENSE"
  cp "$repo_root/README.md" "$fixture/stage/$root/README.md"
  cp "$native_binary" "$fixture/stage/$root/seaf"
  chmod 0644 \
    "$fixture/stage/$root/CHANGELOG.md" \
    "$fixture/stage/$root/LICENSE" \
    "$fixture/stage/$root/README.md"
  chmod 0755 "$fixture/stage/$root/seaf"

  case "$kind" in
    traversal)
      printf 'outside\n' >"$fixture/stage/evil"
      tar --format ustar -cf "$tar_path" -C "$fixture/stage" "$root/../evil"
      ;;
    nonregular)
      rm "$fixture/stage/$root/seaf"
      ln -s README.md "$fixture/stage/$root/seaf"
      tar --format ustar -cf "$tar_path" -C "$fixture/stage" "$root"
      ;;
    duplicate)
      tar --format ustar -cf "$tar_path" -C "$fixture/stage" \
        "$root" "$root/README.md"
      ;;
    wrong-inventory)
      printf 'extra\n' >"$fixture/stage/$root/EXTRA"
      tar --format ustar -cf "$tar_path" -C "$fixture/stage" "$root"
      ;;
    *) fail "unknown bad archive fixture: $kind" ;;
  esac

  gzip -n -c "$tar_path" >"$archive"
}

review_failure_count=0
fixture_archive_index=0

record_review_failure() {
  echo "REVIEW RED: $*" >&2
  review_failure_count=$((review_failure_count + 1))
}

expect_review_rejection() {
  local label="$1"
  local expected="$2"
  shift 2
  local output

  if output="$("$@" 2>&1)"; then
    record_review_failure "$label was accepted"
    return 1
  fi
  if [[ -n "$expected" && "$output" != *"$expected"* ]]; then
    record_review_failure "$label did not report '$expected'; output: $output"
    return 1
  fi
}

create_normalized_fixture_archive() {
  local target="$1"
  local binary="$2"
  local output_directory="$3"
  local root="seaf-v${VERSION}-${target}"
  local fixture stage archive

  fixture_archive_index=$((fixture_archive_index + 1))
  fixture="$temp_root/normalized-fixture-$fixture_archive_index"
  stage="$fixture/stage/$root"
  archive="$output_directory/${root}.tar.gz"
  mkdir -p "$stage"
  install -m 0644 "$repo_root/CHANGELOG.md" "$stage/CHANGELOG.md"
  install -m 0644 "$repo_root/LICENSE" "$stage/LICENSE"
  install -m 0644 "$repo_root/README.md" "$stage/README.md"
  install -m 0755 "$binary" "$stage/seaf"
  chmod 0755 "$stage"
  TZ=UTC touch -t 197001010000.00 \
    "$stage" \
    "$stage/CHANGELOG.md" \
    "$stage/LICENSE" \
    "$stage/README.md" \
    "$stage/seaf"

  if tar --version 2>&1 | grep -q 'GNU tar'; then
    tar \
      --format=ustar \
      --no-recursion \
      --owner=0 \
      --group=0 \
      --numeric-owner \
      --mtime=@0 \
      -cf - \
      -C "$fixture/stage" \
      "$root/" \
      "$root/CHANGELOG.md" \
      "$root/LICENSE" \
      "$root/README.md" \
      "$root/seaf" |
      gzip -n >"$archive"
  else
    COPYFILE_DISABLE=1 tar \
      --format ustar \
      --no-recursion \
      --uid 0 \
      --gid 0 \
      --uname '' \
      --gname '' \
      -cf - \
      -C "$fixture/stage" \
      "$root/" \
      "$root/CHANGELOG.md" \
      "$root/LICENSE" \
      "$root/README.md" \
      "$root/seaf" |
      gzip -n >"$archive"
  fi
}

read_fixture_octal() {
  local tar_path="$1"
  local offset="$2"
  local length="$3"
  local value

  value="$(dd if="$tar_path" bs=1 skip="$offset" count="$length" 2>/dev/null |
    LC_ALL=C tr -d '\000 ')"
  [[ "$value" =~ ^[0-7]+$ ]] || fail "fixture contains malformed octal metadata"
  printf '%s\n' "$((8#$value))"
}

rewrite_fixture_header_checksum() {
  local tar_path="$1"
  local header_offset="$2"
  local checksum sum

  printf '        ' |
    dd of="$tar_path" bs=1 seek="$((header_offset + 148))" conv=notrunc 2>/dev/null
  sum="$(od -An -tu1 -v -j "$header_offset" -N 512 "$tar_path" |
    awk '{ for (field = 1; field <= NF; field++) total += $field } END { print total }')"
  printf -v checksum '%06o' "$sum"
  printf '%s\000 ' "$checksum" |
    dd of="$tar_path" bs=1 seek="$((header_offset + 148))" conv=notrunc 2>/dev/null
}

mutate_archive_metadata() {
  local kind="$1"
  local source_archive="$2"
  local output_archive="$3"
  local fixture tar_path member_header=512 member_size padding_offset
  local header_offset=0 member_index

  fixture_archive_index=$((fixture_archive_index + 1))
  fixture="$temp_root/metadata-mutation-$fixture_archive_index"
  mkdir -p "$fixture" "$(dirname "$output_archive")"

  case "$kind" in
    gzip-xfl)
      cp "$source_archive" "$output_archive"
      printf '\001' |
        dd of="$output_archive" bs=1 seek=8 conv=notrunc 2>/dev/null
      return
      ;;
    gzip-os)
      cp "$source_archive" "$output_archive"
      printf '\000' |
        dd of="$output_archive" bs=1 seek=9 conv=notrunc 2>/dev/null
      return
      ;;
  esac

  tar_path="$fixture/archive.tar"
  gzip -dc "$source_archive" >"$tar_path"
  case "$kind" in
    gnu-tar-null-device-fields)
      for ((member_index = 0; member_index < 5; member_index++)); do
        dd if=/dev/zero of="$tar_path" bs=1 \
          seek="$((header_offset + 329))" count=16 conv=notrunc 2>/dev/null
        rewrite_fixture_header_checksum "$tar_path" "$header_offset"
        member_size="$(read_fixture_octal "$tar_path" "$((header_offset + 124))" 12)"
        header_offset=$((header_offset + 512 + ((member_size + 511) / 512) * 512))
      done
      ;;
    checksum-form)
      printf ' \000' |
        dd of="$tar_path" bs=1 seek="$((member_header + 154))" conv=notrunc 2>/dev/null
      ;;
    checksum-value)
      printf '7' |
        dd of="$tar_path" bs=1 seek="$((member_header + 148))" conv=notrunc 2>/dev/null
      ;;
    linkname)
      printf 'x' |
        dd of="$tar_path" bs=1 seek="$((member_header + 157))" conv=notrunc 2>/dev/null
      rewrite_fixture_header_checksum "$tar_path" "$member_header"
      ;;
    device-major)
      printf '1' |
        dd of="$tar_path" bs=1 seek="$((member_header + 329))" conv=notrunc 2>/dev/null
      rewrite_fixture_header_checksum "$tar_path" "$member_header"
      ;;
    device-major-embedded-null)
      printf '\060\060\000\060\060\060\060\060' |
        dd of="$tar_path" bs=1 seek="$((member_header + 329))" conv=notrunc 2>/dev/null
      rewrite_fixture_header_checksum "$tar_path" "$member_header"
      ;;
    device-minor)
      printf '1' |
        dd of="$tar_path" bs=1 seek="$((member_header + 337))" conv=notrunc 2>/dev/null
      rewrite_fixture_header_checksum "$tar_path" "$member_header"
      ;;
    prefix)
      printf 'x' |
        dd of="$tar_path" bs=1 seek="$((member_header + 345))" conv=notrunc 2>/dev/null
      rewrite_fixture_header_checksum "$tar_path" "$member_header"
      ;;
    reserved)
      printf 'x' |
        dd of="$tar_path" bs=1 seek="$((member_header + 500))" conv=notrunc 2>/dev/null
      rewrite_fixture_header_checksum "$tar_path" "$member_header"
      ;;
    member-padding)
      member_size="$(read_fixture_octal "$tar_path" "$((member_header + 124))" 12)"
      ((member_size % 512 != 0)) || fail "fixture member unexpectedly has no padding"
      padding_offset=$((member_header + 512 + member_size))
      printf 'x' |
        dd of="$tar_path" bs=1 seek="$padding_offset" conv=notrunc 2>/dev/null
      ;;
    *) fail "unknown metadata mutation: $kind" ;;
  esac
  gzip -n -c "$tar_path" >"$output_archive"
}

run_review_regressions() {
  local target="$1"
  local review_root="$temp_root/review-regressions"
  local good_binary="$review_root/good-seaf"
  local exit_binary="$review_root/exit-seven-seaf"
  local stderr_binary="$review_root/stderr-seaf"
  local stdout_overflow_binary="$review_root/stdout-overflow-seaf"
  local stderr_overflow_binary="$review_root/stderr-overflow-seaf"
  local overflow_marker
  local output archive output_listing
  local corrupt_tools="$review_root/corrupt-tools"
  local real_gzip="$(command -v gzip)"
  local marker="$review_root/decompress-completed"
  local input_directory="$review_root/assembly-inputs"
  local assembly_tools="$review_root/assembly-tools"
  local assembly_counter="$review_root/install-count"
  local real_install="$(command -v install)"
  local checksum_tools="$review_root/checksum-tools"
  local checksum_output checksum_result expected_checksum_order
  local mutation mutation_directory mutation_archive

  mkdir -p "$review_root"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'case "${1:-}" in' \
    "  --version) printf 'seaf $VERSION\\n' ;;" \
    "  info) printf 'Self-Evolving Application Framework\\n' ;;" \
    '  *) exit 2 ;;' \
    'esac' >"$good_binary"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'case "${1:-}" in' \
    "  --version) printf 'seaf $VERSION\\n'; exit 7 ;;" \
    "  info) printf 'Self-Evolving Application Framework\\n' ;;" \
    '  *) exit 2 ;;' \
    'esac' >"$exit_binary"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'case "${1:-}" in' \
    "  --version) printf 'seaf $VERSION\\n' ;;" \
    "  info) printf 'Self-Evolving Application Framework\\n'; printf 'unexpected stderr\\n' >&2 ;;" \
    '  *) exit 2 ;;' \
    'esac' >"$stderr_binary"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'case "${1:-}" in' \
    '  --version)' \
    '    dd if=/dev/zero bs=1048576 count=64 2>/dev/null' \
    "    printf 'x'" \
    '    : >"${IDENTITY_COMPLETION_MARKER:?}"' \
    '    ;;' \
    "  info) printf 'Self-Evolving Application Framework\\n' ;;" \
    '  *) exit 2 ;;' \
    'esac' >"$stdout_overflow_binary"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'case "${1:-}" in' \
    '  --version)' \
    "    printf 'seaf $VERSION\\n'" \
    '    dd if=/dev/zero bs=1048576 count=64 >&2 2>/dev/null' \
    "    printf 'x' >&2" \
    '    : >"${IDENTITY_COMPLETION_MARKER:?}"' \
    '    ;;' \
    "  info) printf 'Self-Evolving Application Framework\\n' ;;" \
    '  *) exit 2 ;;' \
    'esac' >"$stderr_overflow_binary"
  chmod 0755 \
    "$good_binary" \
    "$exit_binary" \
    "$stderr_binary" \
    "$stdout_overflow_binary" \
    "$stderr_overflow_binary"

  echo "==> Review regressions: require status/stdout/stderr identity"
  for binary in "$exit_binary" "$stderr_binary"; do
    output="$review_root/build-$(basename "$binary")"
    expect_review_rejection "build identity $(basename "$binary")" "binary identity" \
      "$build_script" build "$target" "$binary" "$output" || true
    if [[ -e "$output" || -L "$output" ]]; then
      record_review_failure "failed build leaked previously absent output $output"
      rm -rf "$output"
    fi

    output="$review_root/verify-$(basename "$binary")"
    mkdir "$output"
    create_normalized_fixture_archive "$target" "$binary" "$output"
    archive="$output/seaf-v${VERSION}-${target}.tar.gz"
    expect_review_rejection "full verify identity $(basename "$binary")" \
      "archived binary identity" \
      "$build_script" verify "$target" "$archive" || true
  done

  for binary in "$stdout_overflow_binary" "$stderr_overflow_binary"; do
    overflow_marker="$review_root/$(basename "$binary").completed"
    output="$review_root/build-$(basename "$binary")"
    expect_review_rejection "bounded build identity $(basename "$binary")" \
      "binary identity" \
      env IDENTITY_COMPLETION_MARKER="$overflow_marker" \
      "$build_script" build "$target" "$binary" "$output" || true
    [[ ! -e "$overflow_marker" ]] ||
      record_review_failure "build identity capture exceeded 64 MiB for $(basename "$binary")"
    if [[ -e "$output" || -L "$output" ]]; then
      record_review_failure "bounded identity failure leaked output $output"
      rm -rf "$output"
    fi

    output="$review_root/verify-$(basename "$binary")"
    mkdir "$output"
    create_normalized_fixture_archive "$target" "$binary" "$output"
    archive="$output/seaf-v${VERSION}-${target}.tar.gz"
    rm -f "$overflow_marker"
    expect_review_rejection "bounded full verify identity $(basename "$binary")" \
      "archived binary identity" \
      env IDENTITY_COMPLETION_MARKER="$overflow_marker" \
      "$build_script" verify "$target" "$archive" || true
    [[ ! -e "$overflow_marker" ]] ||
      record_review_failure "full verify identity capture exceeded 64 MiB for $(basename "$binary")"
  done

  echo "==> Review regressions: restore build outputs after late failure"
  mkdir "$corrupt_tools"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    '"${REAL_GZIP:?}" "$@"' \
    "printf 'tamper'" >"$corrupt_tools/gzip"
  chmod 0755 "$corrupt_tools/gzip"

  output="$review_root/absent-build-output"
  : >"$review_root/build-parent-sentinel"
  expect_review_rejection "late build failure with absent output" "archive inventory" \
    env REAL_GZIP="$real_gzip" PATH="$corrupt_tools:$PATH" \
    "$build_script" build "$target" "$good_binary" "$output" || true
  [[ ! -e "$output" && ! -L "$output" ]] ||
    record_review_failure "late build failure left a script-created output directory"
  [[ -f "$review_root/build-parent-sentinel" ]] ||
    record_review_failure "build cleanup removed a sibling sentinel"
  rm -rf "$output"

  output="$review_root/existing-build-output"
  mkdir "$output"
  : >"$output/caller-sentinel"
  expect_review_rejection "late build failure with caller output" "archive inventory" \
    env REAL_GZIP="$real_gzip" PATH="$corrupt_tools:$PATH" \
    "$build_script" build "$target" "$good_binary" "$output" || true
  [[ -d "$output" && -f "$output/caller-sentinel" ]] ||
    record_review_failure "build cleanup removed caller-owned output state"
  output_listing="$(find "$output" -mindepth 1 -maxdepth 1 -print | sed 's#.*/##')"
  [[ "$output_listing" == "caller-sentinel" ]] ||
    record_review_failure "failed build left an archive in caller-owned output"

  echo "==> Review regressions: enforce the exact 128 MiB decompression cap"
  output="$review_root/decompression-input"
  mkdir "$output"
  create_normalized_fixture_archive "$target" "$good_binary" "$output"
  archive="$output/seaf-v${VERSION}-${target}.tar.gz"
  mkdir "$review_root/decompression-tools"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'set -e' \
    'if [[ "${1:-}" == "-dc" ]]; then' \
    '  dd if=/dev/zero bs=1048576 count=128 2>/dev/null' \
    "  printf 'x'" \
    '  : >"${DECOMPRESSION_MARKER:?}"' \
    '  exit 0' \
    'fi' \
    'exec "${REAL_GZIP:?}" "$@"' >"$review_root/decompression-tools/gzip"
  chmod 0755 "$review_root/decompression-tools/gzip"
  expect_review_rejection "128 MiB decompression boundary" "128 MiB" \
    env DECOMPRESSION_MARKER="$marker" REAL_GZIP="$real_gzip" \
    PATH="$review_root/decompression-tools:$PATH" \
    "$build_script" verify-structure "$target" "$archive" || true
  [[ ! -e "$marker" ]] ||
    record_review_failure "decompressor wrote beyond 128 MiB before rejection"

  echo "==> Review regressions: roll back partially assembled outputs"
  mkdir "$input_directory"
  create_normalized_fixture_archive "$LINUX_TARGET" "$good_binary" "$input_directory"
  create_normalized_fixture_archive "$MACOS_TARGET" "$good_binary" "$input_directory"
  mkdir "$assembly_tools"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'count=0' \
    'if [[ -f "${INSTALL_COUNTER:?}" ]]; then read -r count <"$INSTALL_COUNTER"; fi' \
    'count=$((count + 1))' \
    'printf "%s\\n" "$count" >"$INSTALL_COUNTER"' \
    'if [[ "$count" -eq 2 ]]; then exit 7; fi' \
    'exec "${REAL_INSTALL:?}" "$@"' >"$assembly_tools/install"
  chmod 0755 "$assembly_tools/install"
  output="$review_root/absent-assembly-output"
  : >"$review_root/assembly-parent-sentinel"
  expect_review_rejection "partial assembly failure" "" \
    env INSTALL_COUNTER="$assembly_counter" REAL_INSTALL="$real_install" \
    PATH="$assembly_tools:$PATH" \
    "$assemble_script" assemble "$input_directory" "$output" || true
  [[ ! -e "$output" && ! -L "$output" ]] ||
    record_review_failure "partial assembly left a script-created output directory"
  [[ -f "$review_root/assembly-parent-sentinel" ]] ||
    record_review_failure "assembly cleanup removed a sibling sentinel"
  rm -rf "$output"

  echo "==> Review regression: order SHA256SUMS by archive name, not digest"
  mkdir "$checksum_tools"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'set -euo pipefail' \
    'case "$(basename "${1:?}")" in' \
    "  seaf-v${VERSION}-${LINUX_TARGET}.tar.gz) digest=0000000000000000000000000000000000000000000000000000000000000000 ;;" \
    "  seaf-v${VERSION}-${MACOS_TARGET}.tar.gz) digest=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff ;;" \
    '  *) printf "unexpected checksum input: %s\\n" "$1" >&2; exit 2 ;;' \
    'esac' \
    'printf "%s  %s\\n" "$digest" "$1"' >"$checksum_tools/sha256sum"
  chmod 0755 "$checksum_tools/sha256sum"
  checksum_output="$review_root/checksum-name-order-output"
  if ! checksum_result="$(
    env PATH="$checksum_tools:$PATH" \
      "$assemble_script" assemble "$input_directory" "$checksum_output" 2>&1
  )"; then
    record_review_failure \
      "checksum assembly depends on digest order; output: $checksum_result"
  else
    expected_checksum_order="$(
      printf '%s  %s\n' \
        ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff \
        "seaf-v${VERSION}-${MACOS_TARGET}.tar.gz"
      printf '%s  %s\n' \
        0000000000000000000000000000000000000000000000000000000000000000 \
        "seaf-v${VERSION}-${LINUX_TARGET}.tar.gz"
    )"
    [[ "$(cat "$checksum_output/SHA256SUMS")" == "$expected_checksum_order" ]] ||
      record_review_failure "SHA256SUMS is not ordered lexically by archive name"
  fi

  echo "==> Review regressions: reject noncanonical gzip and USTAR bytes"
  output="$review_root/metadata-source"
  mkdir "$output"
  create_normalized_fixture_archive "$target" "$good_binary" "$output"
  archive="$output/seaf-v${VERSION}-${target}.tar.gz"
  for mutation in \
    gzip-xfl \
    gzip-os \
    checksum-form \
    checksum-value \
    linkname \
    device-major \
    device-minor \
    prefix \
    reserved \
    member-padding; do
    mutation_directory="$review_root/mutation-$mutation"
    mutation_archive="$mutation_directory/$(basename "$archive")"
    mutate_archive_metadata "$mutation" "$archive" "$mutation_archive"
    expect_review_rejection "metadata mutation $mutation" "archive" \
      "$build_script" verify-structure "$target" "$mutation_archive" || true
  done

  echo "==> Review regression: accept GNU tar zero-byte device fields for non-device USTAR entries"
  mutation="gnu-tar-null-device-fields"
  mutation_directory="$review_root/mutation-$mutation"
  mutation_archive="$mutation_directory/$(basename "$archive")"
  mutate_archive_metadata "$mutation" "$archive" "$mutation_archive"
  if ! output="$("$build_script" verify-structure "$target" "$mutation_archive" 2>&1)"; then
    record_review_failure \
      "GNU tar uses zero-byte device fields for non-device USTAR entries; verifier output: $output"
  fi

  echo "==> Review regression: reject embedded NUL in device-major metadata"
  mutation="device-major-embedded-null"
  mutation_directory="$review_root/mutation-$mutation"
  mutation_archive="$mutation_directory/$(basename "$archive")"
  mutate_archive_metadata "$mutation" "$archive" "$mutation_archive"
  expect_review_rejection "embedded-NUL device-major metadata" \
    "malformed device-major metadata" \
    "$build_script" verify-structure "$target" "$mutation_archive" || true

  if ((review_failure_count > 0)); then
    fail "$review_failure_count review regression assertions failed"
  fi
}

require_release_scripts

[[ "$(uname -s)" == "Darwin" || "$(uname -s)" == "Linux" ]] ||
  fail "release artifact test supports only macOS and Linux"

temp_parent="${TMPDIR:-/tmp}"
temp_parent="$(cd "$temp_parent" && pwd -P)"
case "$temp_parent/" in
  "$repo_root/"*) fail "TMPDIR must be outside the source repository" ;;
esac
temp_root="$(mktemp -d "$temp_parent/seaf-release-test.XXXXXX")"
mkdir "$temp_root/script-tmp"
export TMPDIR="$temp_root/script-tmp"

assert_static_release_contract

host_target="$(rustc -vV | sed -n 's/^host: //p')"
case "$(uname -s):$(uname -m):$host_target" in
  "Darwin:arm64:$MACOS_TARGET") ;;
  "Linux:x86_64:$LINUX_TARGET") ;;
  *) fail "local host is not one of the native release matrix rows: $(uname -s) $(uname -m) $host_target" ;;
esac

run_review_regressions "$host_target"
if [[ "$mode" == "--review-regressions-only" ]]; then
  echo "Release artifact review regressions passed."
  exit 0
fi

cargo_target="$temp_root/cargo-target"
mkdir -p "$cargo_target"
echo "==> Build the native release binary with locked Cargo"
CARGO_TARGET_DIR="$cargo_target" \
  cargo build --locked --release -p seaf-cli --bin seaf
native_binary="$cargo_target/release/seaf"
[[ -f "$native_binary" && ! -L "$native_binary" ]] ||
  fail "Cargo did not produce a regular native seaf binary"
(( $(file_size_bytes "$native_binary") <= MAX_FILE_BYTES )) ||
  fail "native seaf binary exceeds the 64 MiB input limit"
assert_exact_output "native version" "seaf $VERSION" "$native_binary" --version
assert_exact_output "native info" "Self-Evolving Application Framework" "$native_binary" info

echo "==> Exercise target, identity, type, and output-root guards"
assert_fails_with "unsupported target" "unsupported release target" \
  "$build_script" build unknown-target "$native_binary" "$temp_root/rejected-target"

wrong_binary="$temp_root/wrong-seaf"
printf '#!/usr/bin/env bash\nprintf "seaf 9.9.9\\n"\n' >"$wrong_binary"
chmod 0755 "$wrong_binary"
assert_fails_with "wrong binary version" "binary identity" \
  "$build_script" build "$host_target" "$wrong_binary" "$temp_root/rejected-version"

binary_link="$temp_root/seaf-link"
ln -s "$native_binary" "$binary_link"
assert_fails_with "symlink binary" "regular non-symlink" \
  "$build_script" build "$host_target" "$binary_link" "$temp_root/rejected-link"

oversize_binary="$temp_root/oversize-seaf"
dd if=/dev/null of="$oversize_binary" bs=1 count=0 seek="$((MAX_FILE_BYTES + 1))" 2>/dev/null
chmod 0755 "$oversize_binary"
assert_fails_with "oversize binary" "64 MiB per-file limit" \
  "$build_script" build "$host_target" "$oversize_binary" "$temp_root/rejected-oversize"

assert_fails_with "repository-local output" "outside the source repository" \
  "$build_script" build "$host_target" "$native_binary" "$repo_root/release-assets"

echo "==> Build the same native archive twice"
first_output="$temp_root/first"
second_output="$temp_root/second"
mkdir -p "$first_output" "$second_output"
archive_name="seaf-v${VERSION}-${host_target}.tar.gz"
first_archive="$first_output/$archive_name"
second_archive="$second_output/$archive_name"
"$build_script" build "$host_target" "$native_binary" "$first_output"
"$build_script" build "$host_target" "$native_binary" "$second_output"
cmp "$first_archive" "$second_archive" ||
  fail "identical inputs did not produce byte-identical archives"
"$build_script" verify "$host_target" "$first_archive"

oversize_archive_dir="$temp_root/oversize-archive"
mkdir "$oversize_archive_dir"
oversize_archive="$oversize_archive_dir/$archive_name"
dd if=/dev/null of="$oversize_archive" bs=1 count=0 seek="$((MAX_FILE_BYTES + 1))" 2>/dev/null
assert_fails_with "oversize archive" "64 MiB per-file limit" \
  "$build_script" verify "$host_target" "$oversize_archive"

[[ "$(od -An -t x1 -N 10 "$first_archive" | tr -d ' \n')" == "1f8b0800000000000003" ]] ||
  fail "gzip header is not normalized to no flags and zero timestamp"

echo "==> Remove a failed archive before it becomes an output"
corrupt_tools="$temp_root/corrupt-tools"
corrupt_output="$temp_root/corrupt-output"
corrupt_archive="$corrupt_output/$archive_name"
real_gzip="$(command -v gzip)"
mkdir "$corrupt_tools" "$corrupt_output"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  '"${REAL_GZIP:?}" "$@"' \
  "printf 'tamper'" >"$corrupt_tools/gzip"
chmod 0755 "$corrupt_tools/gzip"
assert_fails_with "corrupt archive build" "archive inventory" \
  env REAL_GZIP="$real_gzip" PATH="$corrupt_tools:$PATH" \
  "$build_script" build "$host_target" "$native_binary" "$corrupt_output"
[[ ! -e "$corrupt_archive" && ! -L "$corrupt_archive" ]] ||
  fail "failed archive build left a release output behind"

echo "==> Keep aggregate structure verification from executing a foreign binary"
probe_binary="$temp_root/probe-seaf"
probe_marker="$temp_root/probe-executed"
export SEAF_RELEASE_PROBE_MARKER="$probe_marker"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  ': >"${SEAF_RELEASE_PROBE_MARKER:?}"' \
  'case "${1:-}" in' \
  "  --version) printf 'seaf $VERSION\\n' ;;" \
  "  info) printf 'Self-Evolving Application Framework\\n' ;;" \
  '  *) exit 2 ;;' \
  'esac' >"$probe_binary"
chmod 0755 "$probe_binary"
probe_output="$temp_root/probe-output"
mkdir "$probe_output"
"$build_script" build "$host_target" "$probe_binary" "$probe_output" >/dev/null
rm -f "$probe_marker"
"$build_script" verify-structure "$host_target" "$probe_output/$archive_name"
[[ ! -e "$probe_marker" ]] ||
  fail "structure-only verification executed the archived binary"
"$build_script" verify "$host_target" "$probe_output/$archive_name"
[[ -f "$probe_marker" ]] || fail "full native verification did not exercise binary identity"

echo "==> Reject unsafe archive inventories before extraction"
for kind in traversal nonregular duplicate wrong-inventory; do
  bad_dir="$temp_root/bad-output-$kind"
  mkdir -p "$bad_dir"
  bad_archive="$bad_dir/$archive_name"
  make_bad_archive "$kind" "$host_target" "$bad_archive"
  assert_fails_with "$kind archive" "archive inventory" \
    "$build_script" verify "$host_target" "$bad_archive"
done

echo "==> Assemble the exact two-target checksum bundle"
native_inputs="$temp_root/native-inputs"
mkdir -p "$native_inputs"
cp "$first_archive" "$native_inputs/$archive_name"

other_target="$LINUX_TARGET"
[[ "$host_target" == "$LINUX_TARGET" ]] && other_target="$MACOS_TARGET"
other_output="$temp_root/other-output"
mkdir -p "$other_output"
"$build_script" build "$other_target" "$native_binary" "$other_output"
cp "$other_output/seaf-v${VERSION}-${other_target}.tar.gz" "$native_inputs/"

assets="$temp_root/release-assets"
"$assemble_script" assemble "$native_inputs" "$assets"
"$assemble_script" verify "$assets"

asset_listing="$(find "$assets" -mindepth 1 -maxdepth 1 -print | sed 's#.*/##' | LC_ALL=C sort)"
expected_listing="$(printf '%s\n' \
  SHA256SUMS \
  "seaf-v${VERSION}-${LINUX_TARGET}.tar.gz" \
  "seaf-v${VERSION}-${MACOS_TARGET}.tar.gz" | LC_ALL=C sort)"
[[ "$asset_listing" == "$expected_listing" ]] ||
  fail "aggregate release-assets inventory is not exact"

linux_archive="seaf-v${VERSION}-${LINUX_TARGET}.tar.gz"
macos_archive="seaf-v${VERSION}-${MACOS_TARGET}.tar.gz"
expected_sums="$(
  printf '%s  %s\n' "$(sha256_file "$assets/$macos_archive")" "$macos_archive"
  printf '%s  %s\n' "$(sha256_file "$assets/$linux_archive")" "$linux_archive"
  )"
[[ "$(cat "$assets/SHA256SUMS")" == "$expected_sums" ]] ||
  fail "SHA256SUMS content is not exact"
[[ "$(tail -c 1 "$assets/SHA256SUMS" | od -An -t x1 | tr -d ' \n')" == "0a" ]] ||
  fail "SHA256SUMS does not end in exactly one newline"

echo "==> Reject malformed aggregate inputs and tampering"
extra_inputs="$temp_root/extra-inputs"
cp -R "$native_inputs" "$extra_inputs"
printf 'extra\n' >"$extra_inputs/EXTRA"
assert_fails_with "extra aggregate input" "input inventory" \
  "$assemble_script" assemble "$extra_inputs" "$temp_root/rejected-extra"

missing_inputs="$temp_root/missing-inputs"
cp -R "$native_inputs" "$missing_inputs"
rm "$missing_inputs/$linux_archive"
assert_fails_with "missing aggregate input" "input inventory" \
  "$assemble_script" assemble "$missing_inputs" "$temp_root/rejected-missing"

renamed_inputs="$temp_root/renamed-inputs"
cp -R "$native_inputs" "$renamed_inputs"
mv "$renamed_inputs/$linux_archive" "$renamed_inputs/renamed.tar.gz"
assert_fails_with "renamed aggregate input" "input inventory" \
  "$assemble_script" assemble "$renamed_inputs" "$temp_root/rejected-renamed"

assert_fails_with "repository-local aggregate output" "outside the source repository" \
  "$assemble_script" assemble "$native_inputs" "$repo_root/release-assets"

tampered_assets="$temp_root/tampered-assets"
cp -R "$assets" "$tampered_assets"
printf 'tamper' >>"$tampered_assets/$linux_archive"
assert_fails_with "tampered aggregate" "checksum" \
  "$assemble_script" verify "$tampered_assets"

malformed_assets="$temp_root/malformed-assets"
cp -R "$assets" "$malformed_assets"
printf '%s *%s\n' "$(sha256_file "$malformed_assets/$linux_archive")" "$linux_archive" \
  >"$malformed_assets/SHA256SUMS"
assert_fails_with "malformed checksum file" "SHA256SUMS" \
  "$assemble_script" verify "$malformed_assets"

path_checksum_assets="$temp_root/path-checksum-assets"
cp -R "$assets" "$path_checksum_assets"
sed '1s#  #  nested/#' "$assets/SHA256SUMS" >"$path_checksum_assets/SHA256SUMS"
assert_fails_with "path-bearing checksum" "SHA256SUMS" \
  "$assemble_script" verify "$path_checksum_assets"

duplicate_checksum_assets="$temp_root/duplicate-checksum-assets"
cp -R "$assets" "$duplicate_checksum_assets"
head -n 1 "$assets/SHA256SUMS" >"$duplicate_checksum_assets/SHA256SUMS"
head -n 1 "$assets/SHA256SUMS" >>"$duplicate_checksum_assets/SHA256SUMS"
assert_fails_with "duplicate checksum" "SHA256SUMS" \
  "$assemble_script" verify "$duplicate_checksum_assets"

echo "==> Validate before extraction, then install the native archive externally"
"$build_script" verify "$host_target" "$assets/$archive_name"
install_root="$temp_root/install"
extract_root="$temp_root/extract"
mkdir -p "$install_root/bin" "$extract_root"
tar -xzf "$assets/$archive_name" -C "$extract_root"
archive_root="$extract_root/seaf-v${VERSION}-${host_target}"
[[ -d "$archive_root" && ! -L "$archive_root" ]] || fail "archive root was not extracted safely"
for doc in CHANGELOG.md LICENSE README.md; do
  [[ -f "$archive_root/$doc" && ! -L "$archive_root/$doc" ]] ||
    fail "$doc is not an extracted regular file"
  [[ "$(file_mode "$archive_root/$doc")" == "644" ]] ||
    fail "$doc does not have normalized 0644 mode"
done
[[ -f "$archive_root/seaf" && ! -L "$archive_root/seaf" ]] ||
  fail "seaf is not an extracted regular file"
[[ "$(file_mode "$archive_root/seaf")" == "755" ]] ||
  fail "seaf does not have normalized 0755 mode"
install -m 0755 "$archive_root/seaf" "$install_root/bin/seaf"
assert_exact_output "installed version" "seaf $VERSION" "$install_root/bin/seaf" --version
assert_exact_output "installed info" "Self-Evolving Application Framework" "$install_root/bin/seaf" info

echo "Release artifact tests passed."
