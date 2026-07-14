#!/usr/bin/env bash

set -euo pipefail

readonly VERSION="0.1.0"
readonly LINUX_TARGET="x86_64-unknown-linux-gnu"
readonly MACOS_TARGET="aarch64-apple-darwin"
readonly MAX_FILE_BYTES=$((64 * 1024 * 1024))
readonly MAX_AGGREGATE_BYTES=$((128 * 1024 * 1024))

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
build_script="$repo_root/scripts/build-release-artifact.sh"
linux_archive="seaf-v${VERSION}-${LINUX_TARGET}.tar.gz"
macos_archive="seaf-v${VERSION}-${MACOS_TARGET}.tar.gz"
created_output_dir=""

fail() {
  echo "Release asset assembly failed: $*" >&2
  exit 1
}

cleanup() {
  local exit_code=$?
  local output_name

  if ((exit_code != 0)) && [[ -n "$created_output_dir" ]]; then
    for output_name in "$linux_archive" "$macos_archive" SHA256SUMS; do
      if ! rm -f -- "$created_output_dir/$output_name"; then
        echo "Release asset cleanup could not remove $created_output_dir/$output_name" >&2
        exit_code=1
      fi
    done
    if ! rmdir -- "$created_output_dir"; then
      echo "Release asset cleanup could not remove $created_output_dir" >&2
      exit_code=1
    fi
  fi
  exit "$exit_code"
}

trap cleanup EXIT

usage() {
  cat >&2 <<'EOF'
Usage:
  assemble-release-assets.sh assemble <input-directory> <output-directory>
  assemble-release-assets.sh verify <release-assets-directory>
EOF
  exit 2
}

file_size_bytes() {
  local path="$1"

  if stat -f '%z' "$path" >/dev/null 2>&1; then
    stat -f '%z' "$path"
  else
    stat -c '%s' "$path"
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

validate_regular_file() {
  local path="$1"
  local label="$2"
  local size

  if [[ -L "$path" || ! -f "$path" ]]; then
    fail "$label must be a regular non-symlink file: $path"
  fi
  size="$(file_size_bytes "$path")" || fail "cannot measure $label"
  ((size <= MAX_FILE_BYTES)) || fail "$label exceeds the 64 MiB limit: $size"
}

canonical_existing_path() {
  local path="$1"
  local parent

  parent="$(cd "$(dirname "$path")" && pwd -P)" || return 1
  printf '%s/%s\n' "$parent" "$(basename "$path")"
}

validate_external_path() {
  local path="$1"
  local label="$2"
  local canonical

  canonical="$(canonical_existing_path "$path")" ||
    fail "$label parent does not exist: $path"
  case "$canonical" in
    "$repo_root" | "$repo_root"/*)
      fail "$label must be outside the source repository: $canonical"
      ;;
  esac
}

directory_inventory() {
  local directory="$1"

  find "$directory" -mindepth 1 -maxdepth 1 -print |
    sed 's#.*/##' |
    LC_ALL=C sort
}

validate_directory() {
  local directory="$1"
  local label="$2"

  if [[ -L "$directory" || ! -d "$directory" ]]; then
    fail "$label must be a regular directory: $directory"
  fi
  validate_external_path "$directory" "$label"
}

validate_archive_input_inventory() {
  local directory="$1"
  local actual expected

  validate_directory "$directory" "release archive input directory"
  actual="$(directory_inventory "$directory")"
  expected="$(printf '%s\n' "$linux_archive" "$macos_archive" | LC_ALL=C sort)"
  [[ "$actual" == "$expected" ]] ||
    fail "input inventory must contain exactly $linux_archive and $macos_archive"
  validate_regular_file "$directory/$linux_archive" "$linux_archive"
  validate_regular_file "$directory/$macos_archive" "$macos_archive"
}

validate_assets_inventory() {
  local directory="$1"
  local actual expected

  validate_directory "$directory" "release-assets directory"
  actual="$(directory_inventory "$directory")"
  expected="$(printf '%s\n' SHA256SUMS "$linux_archive" "$macos_archive" | LC_ALL=C sort)"
  [[ "$actual" == "$expected" ]] ||
    fail "release-assets inventory is missing, extra, renamed, or duplicated"
  validate_regular_file "$directory/$linux_archive" "$linux_archive"
  validate_regular_file "$directory/$macos_archive" "$macos_archive"
  validate_regular_file "$directory/SHA256SUMS" "SHA256SUMS"
}

expected_checksums() {
  local directory="$1"

  printf '%s  %s\n' "$(sha256_file "$directory/$macos_archive")" "$macos_archive"
  printf '%s  %s\n' "$(sha256_file "$directory/$linux_archive")" "$linux_archive"
}

validate_checksum_shape() {
  local checksum_path="$1"
  local line_count line digest name previous=""

  [[ "$(tail -c 1 "$checksum_path" | od -An -t x1 | tr -d ' \n')" == "0a" ]] ||
    fail "SHA256SUMS must end with one newline"
  line_count="$(wc -l <"$checksum_path" | tr -d ' ')"
  [[ "$line_count" == "2" ]] || fail "SHA256SUMS must contain exactly two lines"

  while IFS= read -r line; do
    [[ "$line" =~ ^([0-9a-f]{64})\ \ ([A-Za-z0-9._-]+)$ ]] ||
      fail "SHA256SUMS has a malformed or path-bearing line"
    digest="${BASH_REMATCH[1]}"
    name="${BASH_REMATCH[2]}"
    [[ "$name" == "$linux_archive" || "$name" == "$macos_archive" ]] ||
      fail "SHA256SUMS names an unexpected archive: $name"
    [[ -z "$previous" || "$previous" < "$name" ]] ||
      fail "SHA256SUMS names are duplicated or not lexically sorted"
    previous="$name"
    [[ -n "$digest" ]] || fail "SHA256SUMS digest is empty"
  done <"$checksum_path"
}

verify_assets() {
  local directory="$1"
  local expected actual total_size

  validate_assets_inventory "$directory"
  validate_checksum_shape "$directory/SHA256SUMS"

  expected="$(expected_checksums "$directory")"
  actual="$(cat "$directory/SHA256SUMS")"
  [[ "$actual" == "$expected" ]] || fail "checksum verification failed"

  total_size=$(($(file_size_bytes "$directory/$linux_archive") + $(file_size_bytes "$directory/$macos_archive") + $(file_size_bytes "$directory/SHA256SUMS")))
  ((total_size <= MAX_AGGREGATE_BYTES)) ||
    fail "release-assets exceed the 128 MiB aggregate limit"

  "$build_script" verify-structure "$LINUX_TARGET" "$directory/$linux_archive" >/dev/null
  "$build_script" verify-structure "$MACOS_TARGET" "$directory/$macos_archive" >/dev/null
}

assemble_assets() {
  local input_directory="$1"
  local output_directory="$2"
  local output_parent

  validate_archive_input_inventory "$input_directory"
  "$build_script" verify-structure \
    "$LINUX_TARGET" "$input_directory/$linux_archive" >/dev/null
  "$build_script" verify-structure \
    "$MACOS_TARGET" "$input_directory/$macos_archive" >/dev/null

  validate_external_path "$output_directory" "release-assets output directory"
  [[ ! -e "$output_directory" && ! -L "$output_directory" ]] ||
    fail "release-assets output directory already exists"
  output_parent="$(cd "$(dirname "$output_directory")" && pwd -P)"
  output_directory="$output_parent/$(basename "$output_directory")"
  mkdir "$output_directory" || fail "cannot create release-assets output directory"
  created_output_dir="$output_directory"

  install -m 0644 "$input_directory/$linux_archive" "$output_directory/$linux_archive"
  install -m 0644 "$input_directory/$macos_archive" "$output_directory/$macos_archive"
  expected_checksums "$output_directory" >"$output_directory/SHA256SUMS"
  chmod 0644 "$output_directory/SHA256SUMS"
  verify_assets "$output_directory"
  created_output_dir=""
  printf '%s\n' "$output_directory"
}

case "${1:-}" in
  assemble)
    [[ $# -eq 3 ]] || usage
    assemble_assets "$2" "$3"
    ;;
  verify)
    [[ $# -eq 2 ]] || usage
    verify_assets "$2"
    ;;
  *) usage ;;
esac
