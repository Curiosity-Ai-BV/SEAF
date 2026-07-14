#!/usr/bin/env bash

set -euo pipefail

readonly EXPECTED_VERSION="0.1.0"
readonly LINUX_TARGET="x86_64-unknown-linux-gnu"
readonly MACOS_TARGET="aarch64-apple-darwin"
readonly MAX_FILE_BYTES=$((64 * 1024 * 1024))
readonly MAX_AGGREGATE_BYTES=$((128 * 1024 * 1024))
readonly EXPECTED_GZIP_HEADER="1f8b0800000000000003"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
temp_root=""
created_output_dir=""
owned_output_path=""

fail() {
  echo "Release artifact failed: $*" >&2
  exit 1
}

cleanup() {
  local exit_code=$?

  if ((exit_code != 0)); then
    if [[ -n "$owned_output_path" ]] && ! rm -f -- "$owned_output_path"; then
      echo "Release artifact cleanup could not remove $owned_output_path" >&2
      exit_code=1
    fi
    if [[ -n "$created_output_dir" ]] && ! rmdir -- "$created_output_dir"; then
      echo "Release artifact cleanup could not remove $created_output_dir" >&2
      exit_code=1
    fi
  fi
  if [[ -n "$temp_root" ]]; then
    rm -rf "$temp_root"
  fi
  exit "$exit_code"
}

trap cleanup EXIT

usage() {
  cat >&2 <<'EOF'
Usage:
  build-release-artifact.sh build <target> <seaf-binary> <output-directory>
  build-release-artifact.sh verify <target> <archive>
  build-release-artifact.sh verify-structure <target> <archive>
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

validate_target() {
  case "$1" in
    "$LINUX_TARGET" | "$MACOS_TARGET") ;;
    *) fail "unsupported release target: $1" ;;
  esac
}

workspace_version() {
  awk '
    /^\[workspace\.package\]$/ { in_workspace_package = 1; next }
    /^\[/ { in_workspace_package = 0 }
    in_workspace_package && /^version = "[^"]+"$/ {
      value = $0
      sub(/^version = "/, "", value)
      sub(/"$/, "", value)
      print value
      count++
    }
    END { if (count != 1) exit 1 }
  ' "$repo_root/Cargo.toml"
}

validate_workspace_version() {
  local actual

  actual="$(workspace_version)" || fail "workspace version is not uniquely readable"
  [[ "$actual" == "$EXPECTED_VERSION" ]] ||
    fail "workspace version must be exactly $EXPECTED_VERSION, found $actual"
}

validate_regular_file() {
  local path="$1"
  local label="$2"
  local size

  if [[ -L "$path" || ! -f "$path" ]]; then
    fail "$label must be a regular non-symlink file: $path"
  fi
  size="$(file_size_bytes "$path")" || fail "cannot measure $label"
  ((size <= MAX_FILE_BYTES)) ||
    fail "$label exceeds the 64 MiB per-file limit: $size"
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

make_temp_root() {
  local temp_parent="${TMPDIR:-/tmp}"

  [[ -L "$temp_parent" || ! -d "$temp_parent" ]] &&
    fail "TMPDIR must be an existing regular directory"
  temp_parent="$(cd "$temp_parent" && pwd -P)" || fail "cannot resolve TMPDIR"
  case "$temp_parent/" in
    "$repo_root/"*) fail "TMPDIR must be outside the source repository" ;;
  esac
  temp_root="$(mktemp -d "$temp_parent/seaf-release-build.XXXXXX")" ||
    fail "cannot create temporary release root"
}

validate_exact_command_identity() {
  local binary="$1"
  local label="$2"
  local expected="$3"
  shift 3
  local command_root="$temp_root/command-identity"
  local stdout_path="$command_root/stdout"
  local stderr_path="$command_root/stderr"
  local expected_path="$command_root/expected"
  local status=0

  rm -rf "$command_root"
  mkdir "$command_root"
  if (
    ulimit -f "$((MAX_FILE_BYTES / 1024))"
    "$binary" "$@" >"$stdout_path" 2>"$stderr_path"
  ); then
    status=0
  else
    status=$?
  fi
  printf '%s\n' "$expected" >"$expected_path"

  ((status == 0)) || fail "$label exited with status $status"
  [[ ! -s "$stderr_path" ]] || fail "$label wrote unexpected stderr"
  cmp -s "$expected_path" "$stdout_path" || fail "$label stdout is not exact"
  rm -rf "$command_root"
}

read_tar_field() {
  local tar_path="$1"
  local offset="$2"
  local length="$3"

  dd if="$tar_path" bs=1 skip="$offset" count="$length" 2>/dev/null |
    LC_ALL=C tr -d '\000'
}

read_tar_octal() {
  local tar_path="$1"
  local offset="$2"
  local length="$3"
  local value

  value="$(read_tar_field "$tar_path" "$offset" "$length" | tr -d ' ')"
  [[ "$value" =~ ^[0-7]+$ ]] || return 1
  printf '%s\n' "$((8#$value))"
}

read_tar_device_number() {
  local tar_path="$1"
  local offset="$2"
  local value raw_hex

  raw_hex="$(od -An -t x1 -j "$offset" -N 8 "$tar_path" | tr -d ' \n')"
  if [[ "$raw_hex" == "0000000000000000" ]]; then
    printf '0\n'
    return
  fi
  if [[ "$raw_hex" =~ ^3[0-7]3[0-7]3[0-7]3[0-7]3[0-7]3[0-7]2000$ ]] ||
    [[ "$raw_hex" =~ ^3[0-7]3[0-7]3[0-7]3[0-7]3[0-7]3[0-7]3[0-7]00$ ]]; then
    value="$(read_tar_field "$tar_path" "$offset" 8 | tr -d ' ')"
    printf '%s\n' "$((8#$value))"
    return
  fi
  return 1
}

validate_ustar_header() {
  local tar_path="$1"
  local offset="$2"
  local expected_name="$3"
  local expected_type="$4"
  local expected_mode="$5"
  local name type magic version uname gname mode uid gid mtime size
  local checksum_hex checksum_digits stored_checksum header_sum checksum_field_sum
  local calculated_checksum linkname_bytes device_major device_minor
  local prefix_bytes reserved_bytes

  name="$(read_tar_field "$tar_path" "$offset" 100)"
  [[ "$name" == "$expected_name" ]] ||
    fail "archive inventory has unexpected header name '$name', expected '$expected_name'"

  mode="$(read_tar_octal "$tar_path" "$((offset + 100))" 8)" ||
    fail "archive inventory has malformed mode metadata for $expected_name"
  uid="$(read_tar_octal "$tar_path" "$((offset + 108))" 8)" ||
    fail "archive inventory has malformed uid metadata for $expected_name"
  gid="$(read_tar_octal "$tar_path" "$((offset + 116))" 8)" ||
    fail "archive inventory has malformed gid metadata for $expected_name"
  size="$(read_tar_octal "$tar_path" "$((offset + 124))" 12)" ||
    fail "archive inventory has malformed size metadata for $expected_name"
  mtime="$(read_tar_octal "$tar_path" "$((offset + 136))" 12)" ||
    fail "archive inventory has malformed timestamp metadata for $expected_name"
  type="$(read_tar_field "$tar_path" "$((offset + 156))" 1)"
  checksum_hex="$(od -An -t x1 -j "$((offset + 148))" -N 8 "$tar_path" | tr -d ' \n')"
  checksum_digits="$(dd if="$tar_path" bs=1 skip="$((offset + 148))" count=6 2>/dev/null)"
  header_sum="$(od -An -tu1 -v -j "$offset" -N 512 "$tar_path" |
    awk '{ for (field = 1; field <= NF; field++) total += $field } END { print total }')"
  checksum_field_sum="$(od -An -tu1 -v -j "$((offset + 148))" -N 8 "$tar_path" |
    awk '{ for (field = 1; field <= NF; field++) total += $field } END { print total }')"
  linkname_bytes="$(dd if="$tar_path" bs=1 skip="$((offset + 157))" count=100 2>/dev/null |
    LC_ALL=C tr -d '\000' | wc -c | tr -d ' ')"
  magic="$(od -An -t x1 -j "$((offset + 257))" -N 6 "$tar_path" | tr -d ' \n')"
  version="$(read_tar_field "$tar_path" "$((offset + 263))" 2)"
  uname="$(read_tar_field "$tar_path" "$((offset + 265))" 32)"
  gname="$(read_tar_field "$tar_path" "$((offset + 297))" 32)"
  device_major="$(read_tar_device_number "$tar_path" "$((offset + 329))")" ||
    fail "archive inventory has malformed device-major metadata for $expected_name"
  device_minor="$(read_tar_device_number "$tar_path" "$((offset + 337))")" ||
    fail "archive inventory has malformed device-minor metadata for $expected_name"
  prefix_bytes="$(dd if="$tar_path" bs=1 skip="$((offset + 345))" count=155 2>/dev/null |
    LC_ALL=C tr -d '\000' | wc -c | tr -d ' ')"
  reserved_bytes="$(dd if="$tar_path" bs=1 skip="$((offset + 500))" count=12 2>/dev/null |
    LC_ALL=C tr -d '\000' | wc -c | tr -d ' ')"

  [[ "$checksum_hex" =~ ^3[0-7]3[0-7]3[0-7]3[0-7]3[0-7]3[0-7]0020$ ]] ||
    fail "archive inventory checksum representation is not canonical for $expected_name"
  stored_checksum=$((8#$checksum_digits))
  calculated_checksum=$((header_sum - checksum_field_sum + 8 * 32))
  [[ "$stored_checksum" == "$calculated_checksum" ]] ||
    fail "archive inventory checksum is invalid for $expected_name"

  [[ "$mode" == "$expected_mode" ]] ||
    fail "archive inventory mode for $expected_name is $mode, expected $expected_mode"
  [[ "$uid" == "0" && "$gid" == "0" && -z "$uname" && -z "$gname" ]] ||
    fail "archive inventory owner metadata is not normalized for $expected_name"
  [[ "$mtime" == "0" ]] ||
    fail "archive inventory timestamp is not normalized for $expected_name"
  [[ "$type" == "$expected_type" ]] ||
    fail "archive inventory contains a nonregular or wrong-type entry: $expected_name"
  [[ "$linkname_bytes" == "0" ]] ||
    fail "archive inventory linkname is not empty for $expected_name"
  [[ "$magic" == "757374617200" && "$version" == "00" ]] ||
    fail "archive inventory is not normalized USTAR for $expected_name"
  [[ "$device_major" == "0" && "$device_minor" == "0" ]] ||
    fail "archive inventory device metadata is not zero for $expected_name"
  [[ "$prefix_bytes" == "0" ]] ||
    fail "archive inventory prefix is not empty for $expected_name"
  [[ "$reserved_bytes" == "0" ]] ||
    fail "archive inventory reserved bytes are not zero for $expected_name"
  ((size <= MAX_FILE_BYTES)) ||
    fail "archive inventory member exceeds the 64 MiB limit: $expected_name"

  printf '%s\n' "$size"
}

verify_archive() {
  local target="$1"
  local archive="$2"
  local smoke_binary="$3"
  local archive_root="seaf-v${EXPECTED_VERSION}-${target}"
  local expected_name="${archive_root}.tar.gz"
  local compressed_size tar_path tar_size listing expected_listing verbose_listing
  local offset=0 aggregate_size=0 member_size tail_size nonzero_tail
  local padding_size nonzero_padding
  local expected_member expected_type expected_mode
  local extracted

  validate_target "$target"
  validate_workspace_version
  validate_regular_file "$archive" "release archive"
  validate_external_path "$archive" "release archive"
  [[ "$(basename "$archive")" == "$expected_name" ]] ||
    fail "release archive name must be exactly $expected_name"
  compressed_size="$(file_size_bytes "$archive")"
  ((compressed_size > 0)) || fail "release archive is empty"

  [[ "$(od -An -t x1 -N 10 "$archive" | tr -d ' \n')" == "$EXPECTED_GZIP_HEADER" ]] ||
    fail "release archive gzip metadata is not normalized"

  make_temp_root
  tar_path="$temp_root/archive.tar"
  if ! (
    ulimit -f "$((MAX_AGGREGATE_BYTES / 1024))"
    gzip -dc "$archive" >"$tar_path"
  ); then
    fail "release archive is invalid or exceeds the 128 MiB expansion limit"
  fi
  tar_size="$(file_size_bytes "$tar_path")"
  ((tar_size <= MAX_AGGREGATE_BYTES)) ||
    fail "release archive exceeds the 128 MiB expansion limit"

  listing="$(tar -tf "$tar_path")" || fail "archive inventory cannot be read"
  expected_listing="$(printf '%s\n' \
    "$archive_root/" \
    "$archive_root/CHANGELOG.md" \
    "$archive_root/LICENSE" \
    "$archive_root/README.md" \
    "$archive_root/seaf")"
  [[ "$listing" == "$expected_listing" ]] ||
    fail "archive inventory is unsafe, duplicated, traversing, or not exact"

  verbose_listing="$(tar --numeric-owner -tvf "$tar_path")" ||
    fail "archive inventory metadata cannot be read"
  [[ "$(printf '%s\n' "$verbose_listing" | wc -l | tr -d ' ')" == "5" ]] ||
    fail "archive inventory metadata is not exact"
  [[ "$(printf '%s\n' "$verbose_listing" | sed -n '1s/ .*//p')" == "drwxr-xr-x" ]] ||
    fail "archive inventory root mode is not normalized"
  [[ "$(printf '%s\n' "$verbose_listing" | sed -n '2s/ .*//p;3s/ .*//p;4s/ .*//p')" == $'-rw-r--r--\n-rw-r--r--\n-rw-r--r--' ]] ||
    fail "archive inventory document modes are not normalized"
  [[ "$(printf '%s\n' "$verbose_listing" | sed -n '5s/ .*//p')" == "-rwxr-xr-x" ]] ||
    fail "archive inventory binary mode is not normalized"

  for expected_member in \
    "$archive_root/" \
    "$archive_root/CHANGELOG.md" \
    "$archive_root/LICENSE" \
    "$archive_root/README.md" \
    "$archive_root/seaf"; do
    expected_type="0"
    expected_mode="420"
    if [[ "$expected_member" == "$archive_root/" ]]; then
      expected_type="5"
      expected_mode="493"
    elif [[ "$expected_member" == "$archive_root/seaf" ]]; then
      expected_mode="493"
    fi

    member_size="$(validate_ustar_header \
      "$tar_path" "$offset" "$expected_member" "$expected_type" "$expected_mode")"
    if [[ "$expected_type" == "5" && "$member_size" != "0" ]]; then
      fail "archive inventory directory has a nonzero size"
    fi
    aggregate_size=$((aggregate_size + member_size))
    ((aggregate_size <= MAX_AGGREGATE_BYTES)) ||
      fail "archive inventory exceeds the 128 MiB aggregate limit"
    padding_size=$((((member_size + 511) / 512) * 512 - member_size))
    if ((padding_size > 0)); then
      nonzero_padding="$(dd \
        if="$tar_path" \
        bs=1 \
        skip="$((offset + 512 + member_size))" \
        count="$padding_size" 2>/dev/null |
        LC_ALL=C tr -d '\000' | wc -c | tr -d ' ')"
      [[ "$nonzero_padding" == "0" ]] ||
        fail "archive inventory member padding is not zero for $expected_member"
    fi
    offset=$((offset + 512 + member_size + padding_size))
  done

  tail_size=$((tar_size - offset))
  ((tail_size >= 1024)) || fail "archive inventory lacks the USTAR end marker"
  nonzero_tail="$(tail -c "+$((offset + 1))" "$tar_path" | LC_ALL=C tr -d '\000' | wc -c | tr -d ' ')"
  [[ "$nonzero_tail" == "0" ]] || fail "archive inventory has data after the final member"

  extracted="$temp_root/extracted"
  mkdir "$extracted"
  for expected_member in CHANGELOG.md LICENSE README.md; do
    tar -xOf "$tar_path" "$archive_root/$expected_member" >"$extracted/$expected_member" ||
      fail "cannot read validated archive member $expected_member"
    validate_regular_file "$extracted/$expected_member" "archived $expected_member"
    cmp "$repo_root/$expected_member" "$extracted/$expected_member" >/dev/null ||
      fail "archived $expected_member does not match the repository input"
  done
  if [[ "$smoke_binary" == "true" ]]; then
    tar -xOf "$tar_path" "$archive_root/seaf" >"$extracted/seaf" ||
      fail "cannot read validated archive binary"
    chmod 0755 "$extracted/seaf"
    validate_regular_file "$extracted/seaf" "archived seaf binary"
    validate_exact_command_identity \
      "$extracted/seaf" \
      "archived binary identity --version" \
      "seaf $EXPECTED_VERSION" \
      --version
    validate_exact_command_identity \
      "$extracted/seaf" \
      "archived binary identity info" \
      "Self-Evolving Application Framework" \
      info
  fi
}

build_archive() {
  local target="$1"
  local binary="$2"
  local output_dir="$3"
  local archive_root="seaf-v${EXPECTED_VERSION}-${target}"
  local archive_name="${archive_root}.tar.gz"
  local output_path stage archive_tmp total_size=0 input size

  validate_target "$target"
  validate_workspace_version
  validate_regular_file "$binary" "seaf binary"
  validate_external_path "$binary" "seaf binary"
  [[ -x "$binary" ]] || fail "seaf binary must be executable"
  make_temp_root
  validate_exact_command_identity \
    "$binary" \
    "binary identity --version" \
    "seaf $EXPECTED_VERSION" \
    --version
  validate_exact_command_identity \
    "$binary" \
    "binary identity info" \
    "Self-Evolving Application Framework" \
    info

  validate_external_path "$output_dir" "release output directory"
  if [[ -e "$output_dir" || -L "$output_dir" ]]; then
    [[ ! -L "$output_dir" && -d "$output_dir" ]] ||
      fail "release output directory must be a regular directory"
  else
    mkdir "$output_dir" || fail "cannot create release output directory"
    created_output_dir="$(cd "$output_dir" && pwd -P)"
  fi
  output_dir="$(cd "$output_dir" && pwd -P)"
  output_path="$output_dir/$archive_name"
  [[ ! -e "$output_path" && ! -L "$output_path" ]] ||
    fail "release output already exists: $output_path"
  owned_output_path="$output_path"

  for input in CHANGELOG.md LICENSE README.md; do
    validate_regular_file "$repo_root/$input" "$input"
    size="$(file_size_bytes "$repo_root/$input")"
    total_size=$((total_size + size))
  done
  size="$(file_size_bytes "$binary")"
  total_size=$((total_size + size))
  ((total_size <= MAX_AGGREGATE_BYTES)) ||
    fail "release inputs exceed the 128 MiB aggregate limit"

  stage="$temp_root/stage/$archive_root"
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

  archive_tmp="$temp_root/$archive_name"
  if tar --version 2>&1 | grep -q 'GNU tar'; then
    tar \
      --format=ustar \
      --no-recursion \
      --owner=0 \
      --group=0 \
      --numeric-owner \
      --mtime=@0 \
      -cf - \
      -C "$temp_root/stage" \
      "$archive_root/" \
      "$archive_root/CHANGELOG.md" \
      "$archive_root/LICENSE" \
      "$archive_root/README.md" \
      "$archive_root/seaf" |
      gzip -n >"$archive_tmp"
  else
    COPYFILE_DISABLE=1 tar \
      --format ustar \
      --no-recursion \
      --uid 0 \
      --gid 0 \
      --uname '' \
      --gname '' \
      -cf - \
      -C "$temp_root/stage" \
      "$archive_root/" \
      "$archive_root/CHANGELOG.md" \
      "$archive_root/LICENSE" \
      "$archive_root/README.md" \
      "$archive_root/seaf" |
      gzip -n >"$archive_tmp"
  fi

  validate_regular_file "$archive_tmp" "release archive output"
  mv "$archive_tmp" "$output_path"
  rm -rf "$temp_root"
  temp_root=""
  if ! "$0" verify "$target" "$output_path"; then
    fail "constructed archive did not pass verification"
  fi
  owned_output_path=""
  created_output_dir=""
  printf '%s\n' "$output_path"
}

case "${1:-}" in
  build)
    [[ $# -eq 4 ]] || usage
    build_archive "$2" "$3" "$4"
    ;;
  verify)
    [[ $# -eq 3 ]] || usage
    verify_archive "$2" "$3" true
    ;;
  verify-structure)
    [[ $# -eq 3 ]] || usage
    verify_archive "$2" "$3" false
    ;;
  *) usage ;;
esac
