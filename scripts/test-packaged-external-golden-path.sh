#!/usr/bin/env bash

set -euo pipefail
export LC_ALL=C

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
fixture_relative_path="fixtures/packaged-external-golden-path/project.txt"
fixture_root="$repo_root/fixtures/packaged-external-golden-path"
build_release_script="$repo_root/scripts/build-release-artifact.sh"
readonly version="0.1.0"
readonly reviewer="golden-path-reviewer@example.invalid"
readonly operator="golden-path-operator"
readonly passing_run_id="packaged-external-passing"
readonly rejection_run_id="packaged-external-rejection"

temp_root=""
source_before_snapshot=""
active_resume_pid=""
active_eval_pid=""

if [[ ! -f "$repo_root/$fixture_relative_path" ]]; then
  echo "Packaged external golden path failed: required fixture file is missing: $fixture_relative_path" >&2
  exit 1
fi

fail() {
  echo "Packaged external golden path failed: $*" >&2
  exit 1
}

run_git() {
  GIT_CONFIG_NOSYSTEM=1 \
    GIT_CONFIG_GLOBAL=/dev/null \
    git \
    -c core.hooksPath=/dev/null \
    -c core.fsmonitor=false \
    "$@"
}

kill_process_group() {
  local process_id="$1"

  [[ "$process_id" =~ ^[1-9][0-9]*$ ]] || return 0
  /bin/kill -KILL "-$process_id" >/dev/null 2>&1 || true
  /bin/kill -KILL "$process_id" >/dev/null 2>&1 || true
}

cleanup() {
  local exit_code=$?
  local source_after_snapshot

  if [[ -n "$active_resume_pid" ]]; then
    /bin/kill -KILL "$active_resume_pid" >/dev/null 2>&1 || true
    wait "$active_resume_pid" >/dev/null 2>&1 || true
  fi
  if [[ -n "$active_eval_pid" ]]; then
    kill_process_group "$active_eval_pid"
  fi

  if [[ -n "$temp_root" && -d "$temp_root" && -n "$source_before_snapshot" ]]; then
    source_after_snapshot="$temp_root/source-after.json"
    if ! snapshot_repository "$repo_root" "$source_after_snapshot"; then
      echo "Packaged external golden path cleanup could not snapshot the source repository" >&2
      exit_code=1
    elif ! cmp -s "$source_before_snapshot" "$source_after_snapshot"; then
      echo "Packaged external golden path changed the SEAF source repository" >&2
      diff -u "$source_before_snapshot" "$source_after_snapshot" >&2 || true
      exit_code=1
    fi
  fi

  if [[ -n "$temp_root" ]]; then
    rm -rf "$temp_root"
  fi
  exit "$exit_code"
}

trap cleanup EXIT

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command is unavailable: $1"
}

file_size_bytes() {
  local path="$1"

  if stat -f '%z' "$path" >/dev/null 2>&1; then
    stat -f '%z' "$path"
  else
    stat -c '%s' "$path"
  fi
}

snapshot_repository() {
  local repository="$1"
  local output="$2"

  node - "$repository" "$output" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const repository = fs.realpathSync(process.argv[2]);
const output = process.argv[3];
const gitEnv = {
  ...process.env,
  GIT_CONFIG_NOSYSTEM: '1',
  GIT_CONFIG_GLOBAL: '/dev/null',
};
function git(args, encoding = 'utf8') {
  return execFileSync('git', ['-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-C', repository, ...args], {
    encoding,
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
}
const listed = git(['ls-files', '-z', '--cached', '--others', '--exclude-standard'], 'buffer')
  .toString('utf8')
  .split('\0')
  .filter(Boolean)
  .sort();
if (listed.length > 4096) throw new Error(`repository inventory exceeds bound: ${listed.length}`);
let aggregate = 0;
const entries = listed.map((relative) => {
  if (path.isAbsolute(relative) || relative.includes('\\') || relative.split('/').some((part) => !part || part === '.' || part === '..')) {
    throw new Error(`unsafe repository path: ${relative}`);
  }
  const absolute = path.join(repository, relative);
  const stat = fs.lstatSync(absolute);
  const base = { path: relative, mode: stat.mode & 0o7777 };
  if (stat.isSymbolicLink()) return { ...base, type: 'symlink', target: fs.readlinkSync(absolute) };
  if (!stat.isFile()) throw new Error(`repository path is not a regular file or symlink: ${relative}`);
  if (stat.size > 64 * 1024 * 1024) throw new Error(`repository file exceeds bound: ${relative}`);
  aggregate += stat.size;
  if (aggregate > 256 * 1024 * 1024) throw new Error('repository aggregate exceeds bound');
  const bytes = fs.readFileSync(absolute);
  return { ...base, type: 'file', size: bytes.length, sha256: crypto.createHash('sha256').update(bytes).digest('hex') };
});
const snapshot = {
  head: git(['rev-parse', 'HEAD']).trim(),
  indexTree: git(['write-tree']).trim(),
  status: git(['status', '--porcelain=v1', '--untracked-files=all']),
  trackedDiffSha256: crypto.createHash('sha256').update(git(['diff', '--binary', '--full-index', '--no-ext-diff', '--no-textconv', 'HEAD', '--'], 'buffer')).digest('hex'),
  cachedDiffSha256: crypto.createHash('sha256').update(git(['diff', '--cached', '--binary', '--full-index', '--no-ext-diff', '--no-textconv', 'HEAD', '--'], 'buffer')).digest('hex'),
  entries,
};
fs.writeFileSync(output, `${JSON.stringify(snapshot)}\n`, { flag: 'wx', mode: 0o600 });
NODE
}

snapshot_directory() {
  local directory="$1"
  local output="$2"
  local prefix="${3:-}"

  node - "$directory" "$output" "$prefix" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const root = fs.realpathSync(process.argv[2]);
const output = process.argv[3];
const prefix = process.argv[4];
const entries = [];
function walk(directory, relativeDirectory = '') {
  for (const name of fs.readdirSync(directory).sort()) {
    const relative = relativeDirectory ? `${relativeDirectory}/${name}` : name;
    const absolute = path.join(directory, name);
    const stat = fs.lstatSync(absolute);
    if (stat.isSymbolicLink()) throw new Error(`directory snapshot contains symlink: ${relative}`);
    if (stat.isDirectory()) {
      walk(absolute, relative);
      continue;
    }
    if (!stat.isFile()) throw new Error(`directory snapshot contains unsafe entry: ${relative}`);
    if (prefix && !relative.startsWith(prefix)) continue;
    if (stat.size > 4 * 1024 * 1024) throw new Error(`snapshot file exceeds bound: ${relative}`);
    const bytes = fs.readFileSync(absolute);
    entries.push({ path: relative, mode: stat.mode & 0o7777, size: bytes.length, sha256: crypto.createHash('sha256').update(bytes).digest('hex') });
  }
}
walk(root);
if (entries.length > 1024) throw new Error(`directory snapshot exceeds bound: ${entries.length}`);
fs.writeFileSync(output, `${JSON.stringify(entries)}\n`, { flag: 'wx', mode: 0o600 });
NODE
}

assert_same_snapshot() {
  local label="$1"
  local expected="$2"
  local actual="$3"

  cmp -s "$expected" "$actual" || {
    diff -u "$expected" "$actual" >&2 || true
    fail "$label changed"
  }
}

assert_rejection_sentinels() {
  local snapshot="$1"

  node - "$snapshot" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');

const snapshot = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const bytes = Buffer.from('packaged rejection preservation sentinel\n', 'utf8');
const file = {
  path: 'rejection-untracked-sentinel.txt',
  mode: 0o640,
  type: 'file',
  size: bytes.length,
  sha256: crypto.createHash('sha256').update(bytes).digest('hex'),
};
const link = {
  path: 'rejection-untracked-sentinel.link',
  mode: 0o777,
  type: 'symlink',
  target: file.path,
};
const entries = new Map(snapshot.entries.map((entry) => [entry.path, entry]));
if (JSON.stringify(entries.get(file.path)) !== JSON.stringify(file)) {
  throw new Error(`rejection regular-file sentinel mismatch: ${JSON.stringify(entries.get(file.path))}`);
}
if (JSON.stringify(entries.get(link.path)) !== JSON.stringify(link)) {
  throw new Error(`rejection symlink sentinel mismatch: ${JSON.stringify(entries.get(link.path))}`);
}
NODE
}

run_in_repository() {
  local label="$1"
  local repository="$2"
  local output="$3"
  shift 3
  local stderr="$output.stderr"

  if ! (cd "$repository" && "$seaf_binary" "$@") >"$output" 2>"$stderr"; then
    echo "$label stderr:" >&2
    sed 's/^/  /' "$stderr" >&2 || true
    fail "$label command failed"
  fi
  if [[ -s "$stderr" ]]; then
    echo "$label stderr:" >&2
    sed 's/^/  /' "$stderr" >&2 || true
    fail "$label wrote unexpected stderr"
  fi
}

run_in_repository_fails_with() {
  local label="$1"
  local repository="$2"
  local output="$3"
  local expected="$4"
  shift 4
  local status

  set +e
  (cd "$repository" && "$seaf_binary" "$@") >"$output" 2>"$output.stderr"
  status=$?
  set -e
  ((status != 0)) || fail "$label unexpectedly succeeded"
  grep -Fq -- "$expected" "$output.stderr" || {
    sed 's/^/  /' "$output.stderr" >&2 || true
    fail "$label did not report the expected failure"
  }
}

run_in_repository_with_eval_cleanup_diagnostic() {
  local label="$1"
  local repository="$2"
  local output="$3"
  shift 3
  local stderr="$output.stderr"

  if ! (cd "$repository" && "$seaf_binary" "$@") >"$output" 2>"$stderr"; then
    echo "$label stderr:" >&2
    sed 's/^/  /' "$stderr" >&2 || true
    fail "$label command failed"
  fi
  node - "$stderr" <<'NODE'
const fs = require('node:fs');
const path = process.argv[2];
const bytes = fs.readFileSync(path);
if (bytes.length > 4096) throw new Error('evaluation cleanup stderr exceeds the 4 KiB bound');
const lines = bytes.toString('utf8').split('\n').filter(Boolean);
const diagnostic = /^(?:\/bin\/)?kill: (?:-[1-9][0-9]*|\(-[1-9][0-9]*\)): No such process$/;
const contract = new Map([
  ['kill: -123: No such process', true],
  ['/bin/kill: -123: No such process', true],
  ['kill: (-123): No such process', true],
  ['/bin/kill: (-123): No such process', true],
  ['kill: (-123: No such process', false],
  ['kill: -123): No such process', false],
  ['unexpected stderr', false],
]);
const mismatches = [...contract].filter(([line, expected]) => diagnostic.test(line) !== expected);
if (mismatches.length !== 0) {
  throw new Error(`evaluation cleanup diagnostic contract mismatch: ${JSON.stringify(mismatches)}`);
}
if (lines.some((line) => !diagnostic.test(line))) {
  throw new Error(`unexpected evaluation cleanup stderr: ${JSON.stringify(lines)}`);
}
NODE
}

json_value() {
  local file="$1"
  local key="$2"

  node -e '
    const fs = require("node:fs");
    let value = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    for (const part of process.argv[2].split(".")) value = value[part];
    if (value === undefined || value === null || typeof value === "object") process.exit(2);
    process.stdout.write(String(value));
  ' "$file" "$key"
}

assert_loop_report() {
  local file="$1"
  local command="$2"
  local run_id="$3"
  local status="$4"

  node - "$file" "$command" "$run_id" "$status" <<'NODE'
const fs = require('node:fs');
const [file, command, runId, status] = process.argv.slice(2);
const report = JSON.parse(fs.readFileSync(file, 'utf8'));
if (report.command !== command || report.run_id !== runId || report.status !== status) {
  throw new Error(`unexpected loop report: ${JSON.stringify(report)}`);
}
if (report.provider !== undefined && (report.provider !== 'fake' || report.model !== 'fake-local')) {
  throw new Error('loop report did not use exact fake-provider authority');
}
NODE
}

assert_inspection() {
  local file="$1"
  local status="$2"
  local crash_boundary="${3:-false}"

  node - "$file" "$status" "$crash_boundary" <<'NODE'
const fs = require('node:fs');
const [file, status, crashBoundary] = process.argv.slice(2);
const report = JSON.parse(fs.readFileSync(file, 'utf8'));
if (report.command !== 'inspect' || report.status !== status || report.integrity !== 'verified') {
  throw new Error(`inspection is not verified at ${status}: ${JSON.stringify(report)}`);
}
for (const [name, value] of Object.entries(report.bounds)) {
  if (name.endsWith('_truncated') && value !== 0) throw new Error(`inspection bound truncated: ${name}`);
}
if (report.integrity_messages.length !== 0 || report.ambiguity_messages.length !== 0) {
  throw new Error('inspection reported integrity or ambiguity messages');
}
for (const input of Object.values(report.input_digests)) {
  if (input.verification !== 'verified') throw new Error(`input is not verified: ${JSON.stringify(input)}`);
}
if (!report.candidate || report.candidate.verification !== 'verified') {
  throw new Error('candidate authority is not verified');
}
for (const attempt of report.provider_attempts) {
  for (const exchange of attempt.exchanges) {
    if (exchange.verification !== 'verified') throw new Error('provider exchange is not verified');
  }
}
if (crashBoundary === 'true') {
  const intent = report.evaluation_prefix.filter((entry) => entry.path === 'artifacts/07-testing.attempt-001.execution-intent.json');
  if (intent.length !== 1 || intent[0].classification !== 'historical') {
    throw new Error('crash boundary did not expose the exact incomplete historical intent');
  }
}
NODE
}

inspect_run() {
  local repository="$1"
  local runs_root="$2"
  local run_id="$3"
  local status="$4"
  local output="$5"
  local crash_boundary="${6:-false}"

  run_in_repository "inspect $run_id" "$repository" "$output" \
    loop inspect --run-id "$run_id" --runs-root "$runs_root" --json
  assert_inspection "$output" "$status" "$crash_boundary"
}

render_eval_config() {
  local destination="$1"
  local mode="$2"
  local control_dir="$3"

  [[ "$mode" == "pass" || "$mode" == "reject" ]] || fail "invalid fixture mode"
  [[ "$control_dir" =~ ^[A-Za-z0-9._/-]+$ ]] || fail "control directory is not template-safe"
  sed \
    -e "s|@SEAF_GOLDEN_PATH_MODE@|$mode|g" \
    -e "s|@SEAF_GOLDEN_PATH_CONTROL_DIR@|$control_dir|g" \
    "$fixture_root/seaf.evals.yaml.in" >"$destination"
}

materialize_repository() {
  local repository="$1"
  local control_dir="$2"
  local mode="$3"
  local label="$4"
  local init_output="$temp_root/$label-init.json"
  local ticket_output="$temp_root/$label-ticket.json"
  local doctor_output="$temp_root/$label-doctor.json"

  mkdir "$repository" "$control_dir"
  chmod 0700 "$repository" "$control_dir"
  install -m 0644 "$fixture_root/project.txt" "$repository/project.txt"
  install -m 0755 "$fixture_root/golden-path-check.sh" "$repository/golden-path-check.sh"
  run_git -C "$repository" init -q
  run_git -C "$repository" config user.name "SEAF Golden Path"
  run_git -C "$repository" config user.email "golden-path@seaf.invalid"

  run_in_repository "$label generic init" "$repository" "$init_output" init --json
  node - "$init_output" <<'NODE'
const fs = require('node:fs');
const report = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const expected = ['seaf.config.json', 'seaf.policy.json', 'seaf.evals.yaml', 'seaf.ticket.yaml', '.seaf/.gitignore'];
if (report.path !== '.' || report.template !== 'generic' || JSON.stringify(report.created) !== JSON.stringify(expected)) {
  throw new Error(`generic init output mismatch: ${JSON.stringify(report)}`);
}
NODE
  for generated in seaf.config.json seaf.policy.json seaf.evals.yaml seaf.ticket.yaml .seaf/.gitignore; do
    [[ -f "$repository/$generated" && ! -L "$repository/$generated" ]] ||
      fail "$label init did not create exact regular output $generated"
  done
  [[ ! -e "$repository/.seaf/loops" ]] || fail "$label init created loop state"

  install -m 0644 "$fixture_root/seaf.ticket.yaml" "$repository/seaf.ticket.yaml"
  render_eval_config "$repository/seaf.evals.yaml" "$mode" "$control_dir"
  run_in_repository "$label ticket validate" "$repository" "$ticket_output" \
    ticket validate seaf.ticket.yaml --json
  node - "$ticket_output" <<'NODE'
const fs = require('node:fs');
const report = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (report.valid !== true || report.kind !== 'ticket' || report.errors.length !== 0) {
  throw new Error(`ticket validation mismatch: ${JSON.stringify(report)}`);
}
NODE

  run_git -C "$repository" add \
    .seaf/.gitignore golden-path-check.sh project.txt \
    seaf.config.json seaf.evals.yaml seaf.policy.json seaf.ticket.yaml
  run_git -C "$repository" commit -q -m "Initialize packaged SEAF fixture"
  [[ -z "$(run_git -C "$repository" status --porcelain=v1 --untracked-files=all)" ]] ||
    fail "$label fixture was not clean after initialization"

  run_in_repository "$label fake doctor" "$repository" "$doctor_output" \
    doctor --provider fake --json
  node - "$doctor_output" <<'NODE'
const fs = require('node:fs');
const report = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (report.schema_version !== 1 || report.ready !== true || report.provider !== 'fake' || report.model !== 'fake-local') {
  throw new Error(`doctor report mismatch: ${JSON.stringify(report)}`);
}
if (!Array.isArray(report.checks) || report.checks.length !== 8 || report.checks.some((check) => check.status !== 'passed')) {
  throw new Error('doctor did not report exactly eight passing checks');
}
NODE
}

wait_for_file() {
  local path="$1"
  local label="$2"
  local attempt

  for attempt in $(seq 1 400); do
    [[ -f "$path" && ! -L "$path" ]] && return 0
    /bin/sleep 0.05
  done
  fail "timed out waiting for $label"
}

assert_provider_count() {
  local run_file="$1"
  local expected="$2"

  node - "$run_file" "$expected" <<'NODE'
const fs = require('node:fs');
const [file, expected] = process.argv.slice(2);
const run = JSON.parse(fs.readFileSync(file, 'utf8'));
if (!Array.isArray(run.provider_exchange_records) || run.provider_exchange_records.length !== Number(expected)) {
  throw new Error('provider exchange ledger length changed');
}
NODE
}

validate_run_artifacts() {
  local run_directory="$1"
  local label="$2"

  node - "$run_directory" "$label" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const runDirectory = fs.realpathSync(process.argv[2]);
const label = process.argv[3];
const MAX_FILES = 4096;
const MAX_DEPTH = 8;
const MAX_FILE_BYTES = 4 * 1024 * 1024;
const MAX_AGGREGATE_BYTES = 32 * 1024 * 1024;
const MAX_REFERENCES = 4096;
const MAX_JSON_DEPTH = 64;
const MAX_JSON_NODES = 65536;
const digestPattern = /^[0-9a-f]{64}$/;
const inventory = new Map();
let aggregateBytes = 0;

function walk(directory, relativeDirectory = '', depth = 0) {
  if (depth > MAX_DEPTH) throw new Error(`${label}: run tree exceeds depth bound`);
  for (const name of fs.readdirSync(directory).sort()) {
    if (!name || name === '.' || name === '..' || name.includes('/') || name.includes('\\')) {
      throw new Error(`${label}: unsafe run entry name`);
    }
    const relative = relativeDirectory ? `${relativeDirectory}/${name}` : name;
    const absolute = path.join(directory, name);
    const stat = fs.lstatSync(absolute);
    if (stat.isSymbolicLink()) throw new Error(`${label}: run tree contains symlink: ${relative}`);
    if (stat.isDirectory()) {
      walk(absolute, relative, depth + 1);
      continue;
    }
    if (!stat.isFile()) throw new Error(`${label}: run tree contains unsafe entry: ${relative}`);
    if (stat.size > MAX_FILE_BYTES) throw new Error(`${label}: run file exceeds bound: ${relative}`);
    aggregateBytes += stat.size;
    if (aggregateBytes > MAX_AGGREGATE_BYTES) throw new Error(`${label}: run tree exceeds aggregate bound`);
    inventory.set(relative, { absolute, size: stat.size });
    if (inventory.size > MAX_FILES) throw new Error(`${label}: run tree exceeds file-count bound`);
  }
}

function safeRelative(reference) {
  return typeof reference === 'string'
    && reference.length > 0
    && !path.posix.isAbsolute(reference)
    && !reference.includes('\\')
    && reference.split('/').every((part) => part && part !== '.' && part !== '..');
}

const references = new Map();
const pending = [];
function addReference(reference, digest, source) {
  if (!safeRelative(reference)) throw new Error(`${label}: unsafe artifact reference from ${source}: ${reference}`);
  if (!digestPattern.test(digest)) throw new Error(`${label}: invalid artifact digest from ${source}: ${reference}`);
  const existing = references.get(reference);
  if (existing && existing !== digest) throw new Error(`${label}: conflicting digests for ${reference}`);
  if (existing) return;
  references.set(reference, digest);
  if (references.size > MAX_REFERENCES) throw new Error(`${label}: artifact reference count exceeds bound`);
  pending.push(reference);
}

let jsonNodes = 0;
function discoverReferences(value, source, depth = 0) {
  if (depth > MAX_JSON_DEPTH) throw new Error(`${label}: JSON reference graph exceeds depth bound at ${source}`);
  jsonNodes += 1;
  if (jsonNodes > MAX_JSON_NODES) throw new Error(`${label}: JSON reference graph exceeds node bound`);
  if (Array.isArray(value)) {
    for (const item of value) discoverReferences(item, source, depth + 1);
    return;
  }
  if (!value || typeof value !== 'object') return;
  if (typeof value.path === 'string' && typeof value.digest === 'string') {
    addReference(value.path, value.digest, source);
  }
  for (const [key, candidatePath] of Object.entries(value)) {
    if (key.endsWith('_path') && typeof candidatePath === 'string') {
      const digestKey = `${key.slice(0, -5)}_digest`;
      if (typeof value[digestKey] === 'string') addReference(candidatePath, value[digestKey], source);
    }
  }
  for (const child of Object.values(value)) discoverReferences(child, source, depth + 1);
}

walk(runDirectory);
const runEntry = inventory.get('run.json');
if (!runEntry) throw new Error(`${label}: run.json is absent`);
const run = JSON.parse(fs.readFileSync(runEntry.absolute, 'utf8'));
discoverReferences(run, 'run.json');
for (const [name, relative] of Object.entries({
  ticket: 'inputs/ticket.json',
  policy: 'inputs/policy.json',
  config: 'inputs/config.json',
  repository: 'inputs/repository.json',
  eval_config: 'inputs/eval-config.json',
})) {
  const digest = run.input_digests && run.input_digests[name];
  if (typeof digest === 'string') addReference(relative, digest, 'run.json input_digests');
}

const parsedJson = new Set();
while (pending.length > 0) {
  const relative = pending.shift();
  const entry = inventory.get(relative);
  if (!entry) throw new Error(`${label}: referenced artifact is absent: ${relative}`);
  const bytes = fs.readFileSync(entry.absolute);
  const observed = crypto.createHash('sha256').update(bytes).digest('hex');
  if (observed !== references.get(relative)) throw new Error(`${label}: artifact digest mismatch: ${relative}`);
  if (relative.endsWith('.json') && !parsedJson.has(relative)) {
    parsedJson.add(relative);
    discoverReferences(JSON.parse(bytes.toString('utf8')), relative);
  }
}

if (references.size === 0) throw new Error(`${label}: no artifact references were validated`);
NODE
}

for command in cargo git gzip node sed tar; do
  require_command "$command"
done
[[ -x "$build_release_script" ]] || fail "release artifact builder is missing or not executable"
for relative in project.txt golden-path-check.sh seaf.ticket.yaml seaf.evals.yaml.in; do
  [[ -f "$fixture_root/$relative" && ! -L "$fixture_root/$relative" ]] ||
    fail "required fixture file is missing or unsafe: fixtures/packaged-external-golden-path/$relative"
done
[[ -x "$fixture_root/golden-path-check.sh" ]] || fail "fixture-native check is not executable"

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) target="aarch64-apple-darwin" ;;
  Linux:x86_64) target="x86_64-unknown-linux-gnu" ;;
  *) fail "unsupported packaged golden-path host: $(uname -s) $(uname -m)" ;;
esac

temp_parent="${TMPDIR:-/tmp}"
[[ -d "$temp_parent" && ! -L "$temp_parent" ]] || fail "TMPDIR is missing or unsafe"
temp_parent="$(cd "$temp_parent" && pwd -P)"
case "$temp_parent/" in
  "$repo_root/"*) fail "TMPDIR must be outside the SEAF source repository" ;;
esac
umask 077
temp_root="$(mktemp -d "$temp_parent/seaf-packaged-golden-path.XXXXXX")" ||
  fail "could not create external temporary root"
[[ "$temp_root" =~ ^[A-Za-z0-9._/-]+$ ]] || fail "temporary root is not fixture-safe"

source_before_snapshot="$temp_root/source-before.json"
snapshot_repository "$repo_root" "$source_before_snapshot"
source_head="$(run_git -C "$repo_root" rev-parse HEAD)"
source_index="$(run_git -C "$repo_root" write-tree)"

build_root="$temp_root/build"
archive_root="$temp_root/archive"
install_root="$temp_root/install"
mkdir "$build_root" "$archive_root" "$install_root"

echo "==> Build and install verified packaged CLI"
(
  cd "$repo_root"
  CARGO_NET_OFFLINE=true cargo build \
    --locked --offline --release --target "$target" \
    --target-dir "$build_root/target"
)
release_binary="$build_root/target/$target/release/seaf"
[[ -f "$release_binary" && ! -L "$release_binary" && -x "$release_binary" ]] ||
  fail "locked release build did not create an executable seaf binary"
archive_path="$archive_root/seaf-v${version}-${target}.tar.gz"
"$build_release_script" build "$target" "$release_binary" "$archive_root" >/dev/null
"$build_release_script" verify "$target" "$archive_path"
tar -xzf "$archive_path" -C "$install_root"
seaf_binary="$install_root/seaf-v${version}-${target}/seaf"
[[ -f "$seaf_binary" && ! -L "$seaf_binary" && -x "$seaf_binary" ]] ||
  fail "verified archive did not install the packaged seaf binary"
"$seaf_binary" --version >"$temp_root/version.stdout" 2>"$temp_root/version.stderr"
printf 'seaf 0.1.0\n' | cmp -s - "$temp_root/version.stdout" || fail "packaged version output mismatch"
[[ ! -s "$temp_root/version.stderr" ]] || fail "packaged version wrote stderr"
"$seaf_binary" info >"$temp_root/info.stdout" 2>"$temp_root/info.stderr"
printf 'Self-Evolving Application Framework\n' | cmp -s - "$temp_root/info.stdout" ||
  fail "packaged info output mismatch"
[[ ! -s "$temp_root/info.stderr" ]] || fail "packaged info wrote stderr"

adoption_started="$(date +%s)"
passing_repo="$temp_root/passing-repo"
passing_control="$temp_root/passing-control"
passing_runs="$temp_root/passing-runs"
rejection_repo="$temp_root/rejection-repo"
rejection_control="$temp_root/rejection-control"
rejection_runs="$temp_root/rejection-runs"

echo "==> Materialize passing external project"
materialize_repository "$passing_repo" "$passing_control" pass passing
passing_source_before="$temp_root/passing-source-before.json"
snapshot_repository "$passing_repo" "$passing_source_before"

passing_run_output="$temp_root/passing-run.json"
run_in_repository "passing loop run" "$passing_repo" "$passing_run_output" \
  loop run --ticket seaf.ticket.yaml --provider fake \
  --runs-root "$passing_runs" --run-id "$passing_run_id" --json
assert_loop_report "$passing_run_output" run "$passing_run_id" awaiting_human_review
candidate_digest="$(json_value "$passing_run_output" candidate_diff_digest)"
target_head="$(json_value "$passing_run_output" target_head)"
[[ "$candidate_digest" =~ ^[0-9a-f]{64}$ ]] || fail "passing run candidate digest is invalid"
[[ "$target_head" =~ ^[0-9a-f]{40,64}$ ]] || fail "passing run target HEAD is invalid"
passing_run_dir="$passing_runs/$passing_run_id"
passing_run_file="$passing_run_dir/run.json"
provider_count="$(node -e 'const r=require(process.argv[1]); process.stdout.write(String(r.provider_exchange_records.length))' "$passing_run_file")"
inspect_run "$passing_repo" "$passing_runs" "$passing_run_id" awaiting_human_review \
  "$temp_root/passing-awaiting-inspect.json"

snapshot_directory "$passing_run_dir" "$temp_root/passing-before-wrong-approval.json"
snapshot_repository "$passing_repo" "$temp_root/passing-before-wrong-source.json"
wrong_digest="$(printf '0%.0s' {1..64})"
run_in_repository_fails_with \
  "wrong candidate approval" "$passing_repo" "$temp_root/wrong-approval.stdout" \
  "confirmed candidate diff digest does not match the current staged candidate diff" \
  loop approve --run-id "$passing_run_id" --runs-root "$passing_runs" \
  --reviewer "$reviewer" --confirm-candidate-diff "$wrong_digest" \
  --confirm-target-head "$target_head" --json
snapshot_directory "$passing_run_dir" "$temp_root/passing-after-wrong-approval.json"
snapshot_repository "$passing_repo" "$temp_root/passing-after-wrong-source.json"
assert_same_snapshot "run authority after wrong approval" \
  "$temp_root/passing-before-wrong-approval.json" "$temp_root/passing-after-wrong-approval.json"
assert_same_snapshot "passing source after wrong approval" \
  "$temp_root/passing-before-wrong-source.json" "$temp_root/passing-after-wrong-source.json"

passing_approve_output="$temp_root/passing-approve.json"
run_in_repository "exact passing approval" "$passing_repo" "$passing_approve_output" \
  loop approve --run-id "$passing_run_id" --runs-root "$passing_runs" \
  --reviewer "$reviewer" --confirm-candidate-diff "$candidate_digest" \
  --confirm-target-head "$target_head" --json
assert_loop_report "$passing_approve_output" approve "$passing_run_id" approved
[[ "$(json_value "$passing_approve_output" testing_ran)" == "false" ]] ||
  fail "approval unexpectedly ran Testing"
inspect_run "$passing_repo" "$passing_runs" "$passing_run_id" approved \
  "$temp_root/passing-approved-inspect.json"

echo "==> Interrupt packaged evaluation and recover with a new indexed attempt"
passing_resume_output="$temp_root/passing-resume-interrupted.json"
(
  cd "$passing_repo"
  exec "$seaf_binary" loop resume --run-id "$passing_run_id" \
    --runs-root "$passing_runs" --json
) >"$passing_resume_output" 2>"$passing_resume_output.stderr" &
active_resume_pid=$!
wait_for_file "$passing_control/started" "fixture evaluation start marker"
wait_for_file "$passing_control/eval.pid" "fixture evaluation PID marker"
active_eval_pid="$(tr -d '\r\n' <"$passing_control/eval.pid")"
[[ "$active_eval_pid" =~ ^[1-9][0-9]*$ ]] || fail "fixture evaluation PID marker is invalid"
[[ -f "$passing_run_dir/artifacts/07-testing.attempt-001.execution-intent.json" ]] ||
  fail "evaluation started without a durable attempt-1 execution intent"
/bin/kill -KILL "$active_resume_pid" || fail "could not interrupt the packaged CLI"
kill_process_group "$active_eval_pid"
set +e
wait "$active_resume_pid"
resume_status=$?
set -e
active_resume_pid=""
active_eval_pid=""
((resume_status != 0)) || fail "interrupted packaged CLI unexpectedly succeeded"
snapshot_repository "$passing_repo" "$temp_root/passing-after-interruption-source.json"
assert_same_snapshot "passing source after real interruption" \
  "$passing_source_before" "$temp_root/passing-after-interruption-source.json"
snapshot_directory "$passing_run_dir" "$temp_root/passing-attempt-one-before-recovery.json" \
  "artifacts/07-testing.attempt-001"
inspect_run "$passing_repo" "$passing_runs" "$passing_run_id" approved \
  "$temp_root/passing-interrupted-inspect.json" true

snapshot_directory "$passing_run_dir" "$temp_root/passing-before-ordinary-resume.json"
run_in_repository_fails_with \
  "ordinary incomplete evaluation resume" "$passing_repo" "$temp_root/ordinary-resume.stdout" \
  "an incomplete Approved evaluation attempt exists; audited recovery is required" \
  loop resume --run-id "$passing_run_id" --runs-root "$passing_runs" --json
snapshot_directory "$passing_run_dir" "$temp_root/passing-after-ordinary-resume.json"
assert_same_snapshot "run authority after rejected ordinary resume" \
  "$temp_root/passing-before-ordinary-resume.json" "$temp_root/passing-after-ordinary-resume.json"

passing_revise_output="$temp_root/passing-revise.json"
run_in_repository "invalidate incomplete evaluation" "$passing_repo" "$passing_revise_output" \
  loop revise --run-id "$passing_run_id" --runs-root "$passing_runs" \
  --from-step testing --eval-recovery invalidate --actor "$operator" \
  --reason "recover packaged golden-path interruption" --json
node - "$passing_revise_output" <<'NODE'
const fs = require('node:fs');
const report = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (report.command !== 'revise' || report.recovery_id !== 1 || report.invalidated_attempt !== 1 || report.next_evaluation_attempt !== 2) {
  throw new Error(`evaluation invalidation mismatch: ${JSON.stringify(report)}`);
}
NODE
snapshot_directory "$passing_run_dir" "$temp_root/passing-attempt-one-after-revise.json" \
  "artifacts/07-testing.attempt-001"
assert_same_snapshot "evaluation attempt 1 after revise" \
  "$temp_root/passing-attempt-one-before-recovery.json" "$temp_root/passing-attempt-one-after-revise.json"
assert_provider_count "$passing_run_file" "$provider_count"
snapshot_repository "$passing_repo" "$temp_root/passing-after-revise-source.json"
assert_same_snapshot "passing source after invalidation" \
  "$passing_source_before" "$temp_root/passing-after-revise-source.json"

: >"$passing_control/release"
passing_rerun_output="$temp_root/passing-rerun.json"
run_in_repository_with_eval_cleanup_diagnostic \
  "rerun invalidated evaluation" "$passing_repo" "$passing_rerun_output" \
  loop rerun --run-id "$passing_run_id" --runs-root "$passing_runs" \
  --recovery 1 --json
assert_loop_report "$passing_rerun_output" rerun "$passing_run_id" eval_passed
assert_provider_count "$passing_run_file" "$provider_count"
snapshot_directory "$passing_run_dir" "$temp_root/passing-attempt-one-after-rerun.json" \
  "artifacts/07-testing.attempt-001"
assert_same_snapshot "evaluation attempt 1 after rerun" \
  "$temp_root/passing-attempt-one-before-recovery.json" "$temp_root/passing-attempt-one-after-rerun.json"
for relative in \
  artifacts/07-testing.attempt-002.execution-intent.json \
  artifacts/07-testing.attempt-002.check-001.stdout.log \
  artifacts/07-testing.attempt-002.check-001.stderr.log \
  artifacts/07-testing.attempt-002.json \
  artifacts/08-eval-report.attempt-002.json; do
  [[ -f "$passing_run_dir/$relative" && ! -L "$passing_run_dir/$relative" ]] ||
    fail "recovered evaluation is missing $relative"
done
printf 'packaged external native check passed\n' |
  cmp -s - "$passing_run_dir/artifacts/07-testing.attempt-002.check-001.stdout.log" ||
  fail "recovered native check stdout mismatch"
[[ ! -s "$passing_run_dir/artifacts/07-testing.attempt-002.check-001.stderr.log" ]] ||
  fail "recovered native check wrote stderr"
snapshot_repository "$passing_repo" "$temp_root/passing-after-rerun-source.json"
assert_same_snapshot "passing source after evaluation recovery" \
  "$passing_source_before" "$temp_root/passing-after-rerun-source.json"
inspect_run "$passing_repo" "$passing_runs" "$passing_run_id" eval_passed \
  "$temp_root/passing-eval-passed-inspect.json"

passing_status_output="$temp_root/passing-status.json"
run_in_repository "passing promotion status" "$passing_repo" "$passing_status_output" \
  loop status --run-id "$passing_run_id" --runs-root "$passing_runs" --json
assert_loop_report "$passing_status_output" status "$passing_run_id" eval_passed
promotion_candidate="$(json_value "$passing_status_output" candidate_diff_digest)"
promotion_eval="$(json_value "$passing_status_output" eval_report_digest)"
promotion_head="$(json_value "$passing_status_output" target_head)"
[[ "$promotion_candidate" == "$candidate_digest" ]] || fail "promotion candidate digest drifted"
[[ "$promotion_eval" =~ ^[0-9a-f]{64}$ ]] || fail "promotion EvalReport digest is invalid"
[[ "$promotion_head" == "$target_head" ]] || fail "promotion target HEAD drifted"

passing_promote_output="$temp_root/passing-promote.json"
run_in_repository "exact passing promotion" "$passing_repo" "$passing_promote_output" \
  loop promote --run-id "$passing_run_id" --runs-root "$passing_runs" \
  --reviewer "$reviewer" --confirm-candidate-diff "$promotion_candidate" \
  --confirm-eval-report "$promotion_eval" --confirm-target-head "$promotion_head" --json
assert_loop_report "$passing_promote_output" promote "$passing_run_id" promoted
[[ "$(run_git -C "$passing_repo" rev-parse HEAD)" == "$target_head" ]] ||
  fail "promotion changed target HEAD"
[[ "$(run_git -C "$passing_repo" write-tree)" == "$(node -e 'process.stdout.write(JSON.parse(require("node:fs").readFileSync(process.argv[1],"utf8")).indexTree)' "$passing_source_before")" ]] ||
  fail "promotion changed the target index"
[[ -z "$(run_git -C "$passing_repo" diff --cached --name-only)" ]] ||
  fail "promotion staged the approved patch"
expected_status='?? examples/local-loop/evals/fake-provider-smoke.txt'
[[ "$(run_git -C "$passing_repo" status --porcelain=v1 --untracked-files=all)" == "$expected_status" ]] ||
  fail "promotion left changes other than the approved unstaged patch"
printf 'provider-backed smoke\n' |
  cmp -s - "$passing_repo/examples/local-loop/evals/fake-provider-smoke.txt" ||
  fail "promoted source bytes do not match the fake-provider candidate"
candidate_path="$(node -e 'const r=require(process.argv[1]); process.stdout.write(r.candidate_workspace.path)' "$passing_run_file")"
cmp -s \
  "$passing_repo/examples/local-loop/evals/fake-provider-smoke.txt" \
  "$candidate_path/examples/local-loop/evals/fake-provider-smoke.txt" ||
  fail "promoted source bytes differ from the frozen candidate"
candidate_diff_path="$(json_value "$passing_status_output" candidate_diff_path)"
run_git -C "$candidate_path" diff \
  --cached --binary --full-index --no-ext-diff --no-textconv HEAD -- \
  >"$temp_root/frozen-candidate.diff"
cmp -s "$passing_run_dir/$candidate_diff_path" "$temp_root/frozen-candidate.diff" ||
  fail "frozen candidate diff does not match the approved artifact"
inspect_run "$passing_repo" "$passing_runs" "$passing_run_id" promoted \
  "$temp_root/passing-promoted-inspect.json"
validate_run_artifacts "$passing_run_dir" "promoted passing run"

adoption_finished="$(date +%s)"
adoption_seconds=$((adoption_finished - adoption_started))
((adoption_seconds < 900)) || fail "post-install adoption scenario exceeded 15 minutes: ${adoption_seconds}s"

echo "==> Materialize deterministic rejection project"
materialize_repository "$rejection_repo" "$rejection_control" reject rejection
rejection_source_before="$temp_root/rejection-source-before.json"
snapshot_repository "$rejection_repo" "$rejection_source_before"

rejection_run_output="$temp_root/rejection-run.json"
run_in_repository "rejection loop run" "$rejection_repo" "$rejection_run_output" \
  loop run --ticket seaf.ticket.yaml --provider fake \
  --runs-root "$rejection_runs" --run-id "$rejection_run_id" --json
assert_loop_report "$rejection_run_output" run "$rejection_run_id" awaiting_human_review
rejection_candidate="$(json_value "$rejection_run_output" candidate_diff_digest)"
rejection_head="$(json_value "$rejection_run_output" target_head)"
rejection_run_dir="$rejection_runs/$rejection_run_id"
rejection_run_file="$rejection_run_dir/run.json"

rejection_approve_output="$temp_root/rejection-approve.json"
run_in_repository "exact rejection approval" "$rejection_repo" "$rejection_approve_output" \
  loop approve --run-id "$rejection_run_id" --runs-root "$rejection_runs" \
  --reviewer "$reviewer" --confirm-candidate-diff "$rejection_candidate" \
  --confirm-target-head "$rejection_head" --json
assert_loop_report "$rejection_approve_output" approve "$rejection_run_id" approved
snapshot_repository "$rejection_repo" "$temp_root/rejection-after-approval-clean.json"
assert_same_snapshot "rejection clean source after run and approval" \
  "$rejection_source_before" "$temp_root/rejection-after-approval-clean.json"
printf 'packaged rejection preservation sentinel\n' \
  >"$rejection_repo/rejection-untracked-sentinel.txt"
chmod 0640 "$rejection_repo/rejection-untracked-sentinel.txt"
(
  umask 000
  ln -s rejection-untracked-sentinel.txt \
    "$rejection_repo/rejection-untracked-sentinel.link"
)
rejection_source_with_sentinels="$temp_root/rejection-source-with-sentinels.json"
snapshot_repository "$rejection_repo" "$rejection_source_with_sentinels"
assert_rejection_sentinels "$rejection_source_with_sentinels"

rejection_resume_output="$temp_root/rejection-resume.json"
run_in_repository_with_eval_cleanup_diagnostic \
  "deterministic rejecting evaluation" "$rejection_repo" "$rejection_resume_output" \
  loop resume --run-id "$rejection_run_id" --runs-root "$rejection_runs" --json
assert_loop_report "$rejection_resume_output" resume "$rejection_run_id" failed
rejection_report="$rejection_run_dir/artifacts/08-eval-report.attempt-001.json"
node - "$rejection_report" "$rejection_run_dir" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const report = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const runDirectory = fs.realpathSync(process.argv[3]);
if (!Array.isArray(report.checks) || report.checks.length !== 1) {
  throw new Error(`rejecting EvalReport check count mismatch: ${JSON.stringify(report)}`);
}
const check = report.checks[0];
if (report.passed !== false
  || report.decision !== 'reject'
  || check.status !== 'failed'
  || check.name !== 'packaged_external_native_check'
  || check.summary !== 'command exited with code 24') {
  throw new Error(`rejecting EvalReport mismatch: ${JSON.stringify(report)}`);
}
function readReferencedLog(relative, digest, label) {
  if (typeof relative !== 'string'
    || path.posix.isAbsolute(relative)
    || relative.includes('\\')
    || relative.split('/').some((part) => !part || part === '.' || part === '..')) {
    throw new Error(`rejecting EvalReport has unsafe ${label} path: ${relative}`);
  }
  const absolute = path.resolve(runDirectory, relative);
  const real = fs.realpathSync(absolute);
  if (real !== absolute || !real.startsWith(`${runDirectory}${path.sep}`)) {
    throw new Error(`rejecting EvalReport ${label} escapes the run: ${relative}`);
  }
  const stat = fs.lstatSync(absolute);
  if (!stat.isFile() || stat.isSymbolicLink()) {
    throw new Error(`rejecting EvalReport ${label} is not a regular file: ${relative}`);
  }
  const bytes = fs.readFileSync(absolute);
  const observed = crypto.createHash('sha256').update(bytes).digest('hex');
  if (digest !== observed) {
    throw new Error(`rejecting EvalReport ${label} digest mismatch`);
  }
  return bytes;
}
const stdout = readReferencedLog(check.stdout_path, check.stdout_digest, 'stdout');
const stderr = readReferencedLog(check.stderr_path, check.stderr_digest, 'stderr');
if (stdout.length !== 0) {
  throw new Error(`rejecting native check wrote stdout: ${JSON.stringify(stdout.toString('utf8'))}`);
}
const expectedStderr = Buffer.from('golden-path check: deterministic rejection requested\n', 'utf8');
if (!stderr.equals(expectedStderr)) {
  throw new Error(`rejecting native check stderr mismatch: ${JSON.stringify(stderr.toString('utf8'))}`);
}
NODE
snapshot_repository "$rejection_repo" "$temp_root/rejection-after-failure.json"
assert_same_snapshot "rejection source after failed evaluation" \
  "$rejection_source_with_sentinels" "$temp_root/rejection-after-failure.json"
inspect_run "$rejection_repo" "$rejection_runs" "$rejection_run_id" failed \
  "$temp_root/rejection-failed-inspect.json"
validate_run_artifacts "$rejection_run_dir" "failed rejection run"

rejection_cleanup_output="$temp_root/rejection-cleanup.json"
run_in_repository "rejection candidate cleanup" "$rejection_repo" "$rejection_cleanup_output" \
  loop cleanup --run-id "$rejection_run_id" --runs-root "$rejection_runs" --json
node - "$rejection_cleanup_output" <<'NODE'
const fs = require('node:fs');
const report = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (report.command !== 'cleanup' || report.status !== 'failed' || report.candidate_lifecycle !== 'cleaned') {
  throw new Error(`cleanup report mismatch: ${JSON.stringify(report)}`);
}
NODE
snapshot_repository "$rejection_repo" "$temp_root/rejection-after-cleanup.json"
assert_same_snapshot "rejection source after cleanup" \
  "$rejection_source_with_sentinels" "$temp_root/rejection-after-cleanup.json"
inspect_run "$rejection_repo" "$rejection_runs" "$rejection_run_id" failed \
  "$temp_root/rejection-cleaned-inspect.json"
validate_run_artifacts "$rejection_run_dir" "cleaned rejection run"

[[ "$(run_git -C "$repo_root" rev-parse HEAD)" == "$source_head" ]] ||
  fail "SEAF source HEAD changed during acceptance"
[[ "$(run_git -C "$repo_root" write-tree)" == "$source_index" ]] ||
  fail "SEAF source index changed during acceptance"
snapshot_repository "$repo_root" "$temp_root/source-final.json"
assert_same_snapshot "SEAF source repository" "$source_before_snapshot" "$temp_root/source-final.json"

echo "Packaged external golden path passed (post-install adoption ${adoption_seconds}s)."
