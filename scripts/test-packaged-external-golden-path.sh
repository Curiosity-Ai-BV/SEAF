#!/usr/bin/env bash

set -euo pipefail
export LC_ALL=C

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
fixture_relative_path="fixtures/packaged-external-golden-path/project.txt"
fixture_root="$repo_root/fixtures/packaged-external-golden-path"
ollama_fixture_root="$fixture_root/ollama"
build_release_script="$repo_root/scripts/build-release-artifact.sh"
readonly version="0.1.0"
readonly reviewer="golden-path-reviewer@example.invalid"
readonly operator="golden-path-operator"
readonly passing_run_id="packaged-external-passing"
readonly rejection_run_id="packaged-external-rejection"
readonly ollama_passing_run_id="packaged-ollama-passing"
readonly ollama_rejection_run_id="packaged-ollama-rejection"
readonly ollama_host="http://localhost:11434"
readonly ollama_base_url="http://localhost:11434/api"
readonly role_timeout_ms="120000"

temp_root=""
source_before_snapshot=""
active_resume_pid=""
active_eval_pid=""
evidence_temp=""
acceptance_mode="fake"
model=""
evidence_out=""
ollama_command=""
ollama_server_version=""
ollama_client_version=""

fail() {
  echo "Packaged external golden path failed: $*" >&2
  exit 1
}

while (($# > 0)); do
  case "$1" in
    --local-live-ollama)
      [[ "$acceptance_mode" == "fake" ]] || fail "--local-live-ollama may be passed only once"
      acceptance_mode="ollama"
      shift
      ;;
    --model)
      [[ -z "$model" ]] || fail "--model may be passed only once"
      (($# >= 2)) || fail "--model requires a value"
      [[ -n "$2" && "$2" != --* ]] || fail "--model requires a value"
      model="$2"
      shift 2
      ;;
    --evidence-out)
      [[ -z "$evidence_out" ]] || fail "--evidence-out may be passed only once"
      (($# >= 2)) || fail "--evidence-out requires a value"
      [[ -n "$2" && "$2" != --* ]] || fail "--evidence-out requires a value"
      evidence_out="$2"
      shift 2
      ;;
    *) fail "unknown argument: $1" ;;
  esac
done

if [[ "$acceptance_mode" == "fake" ]]; then
  [[ -z "$model" ]] || fail "--model requires --local-live-ollama"
  [[ -z "$evidence_out" ]] || fail "--evidence-out requires --local-live-ollama"
else
  [[ -n "$model" ]] || fail "--local-live-ollama requires --model"
  [[ -n "$evidence_out" ]] || fail "--local-live-ollama requires --evidence-out"
  case "${CI:-}" in
    true | TRUE | True | 1 | yes | YES | Yes)
      fail "local Ollama acceptance refuses truthy CI"
      ;;
  esac
  [[ "$evidence_out" == /* ]] || fail "--evidence-out must be an absolute external path"
  [[ ! -e "$evidence_out" && ! -L "$evidence_out" ]] || fail "--evidence-out must not already exist"
  [[ -t 0 ]] || fail "local Ollama acceptance requires an interactive terminal for human review"
  [[ -t 1 ]] || fail "local Ollama acceptance requires interactive stdout to display review authority"
fi

if [[ ! -f "$repo_root/$fixture_relative_path" ]]; then
  fail "required fixture file is missing: $fixture_relative_path"
fi

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
  if [[ -n "$evidence_temp" ]]; then
    rm -f -- "$evidence_temp" || exit_code=1
    evidence_temp=""
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

validate_ollama_review_fixture() {
  node - \
    "$ollama_fixture_root/seaf.ticket.yaml" \
    "$ollama_fixture_root/ollama-acceptance.txt" \
    "$ollama_fixture_root/ollama-native-check.sh" <<'NODE'
const fs = require('node:fs');

const [ticketPath, initialPath, checkPath] = process.argv.slice(2);
function readBoundedRegularFile(path, maximumBytes) {
  const stat = fs.lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink()) {
    throw new Error(`unsafe Ollama review fixture file: ${path}`);
  }
  if (stat.size > maximumBytes) {
    throw new Error(`Ollama review fixture file exceeds bound: ${path}`);
  }
  return fs.readFileSync(path);
}

const ticketBytes = readBoundedRegularFile(ticketPath, 16 * 1024);
const initialBytes = readBoundedRegularFile(initialPath, 64);
const checkBytes = readBoundedRegularFile(checkPath, 16 * 1024);
const ticket = ticketBytes.toString('utf8');
if (!Buffer.from(ticket, 'utf8').equals(ticketBytes)) {
  throw new Error('Ollama ticket is not valid UTF-8');
}
const normalizedTicket = ticket.replace(/\s+/g, ' ').trim();
const initialInstruction = 'When a Spec Creation provider request has no revision_context, the specification must propose the exact final bytes SEAF packaged Ollama acceptance draft. followed by one newline.';
const reviewInstruction = 'This proposed final text is intentionally wrong, so Spec Review must request changes.';
const recoveredInstruction = 'When a Spec Creation provider request has revision_context, the specification must correct the proposed final bytes to SEAF packaged Ollama acceptance passed. followed by one newline.';
const preservationInstruction = 'The recovered specification must preserve the permitted path, policy scope, command, and acceptance criteria.';
for (const instruction of [initialInstruction, reviewInstruction, recoveredInstruction, preservationInstruction]) {
  if (!normalizedTicket.includes(instruction)) {
    throw new Error(`Ollama ticket is missing the trusted review protocol: ${instruction}`);
  }
}
const finalCriterion = '  - After the change its exact bytes are SEAF packaged Ollama acceptance passed. followed by one newline.';
if (!ticket.split('\n').includes(finalCriterion)) {
  throw new Error('Ollama ticket final-byte acceptance criterion changed');
}
if (!initialBytes.equals(Buffer.from('PENDING\n', 'utf8'))) {
  throw new Error('Ollama fixture initial bytes changed');
}
const check = checkBytes.toString('utf8');
if (!check.includes('expected_content="SEAF packaged Ollama acceptance passed."')
  || !check.includes('wc -l <"$candidate_file"')) {
  throw new Error('Ollama fixture native check no longer enforces the exact final bytes and newline');
}
NODE
}

file_size_bytes() {
  local path="$1"

  if stat -f '%z' "$path" >/dev/null 2>&1; then
    stat -f '%z' "$path"
  else
    stat -c '%s' "$path"
  fi
}

report_local_failure() {
  local stderr="$1"

  node - "$stderr" <<'NODE'
const fs = require('node:fs');
const bytes = fs.readFileSync(process.argv[2]);
if (bytes.length > 64 * 1024) {
  process.stderr.write('local Ollama failure category: stderr exceeded the diagnostic bound; raw content omitted\n');
  process.exit(0);
}
const text = bytes.toString('utf8');
const categories = [
  [/failed to parse ([A-Za-z]+) provider response/i, (match) => `role-schema failure at ${match[1]}`],
  [/provider repair request failed for ([A-Za-z]+)/i, (match) => `structured repair failure at ${match[1]}`],
  [/provider request failed for ([A-Za-z]+)/i, (match) => `provider request failure at ${match[1]}`],
  [/request timed out/i, () => 'bounded local provider timeout'],
  [/policy[^\n]*rejected/i, () => 'candidate policy rejection'],
  [/request_changes|request changes/i, () => 'model reviewer requested changes'],
  [/\breject(?:ed|ion)?\b/i, () => 'model reviewer rejected the candidate'],
  [/\bblocked\b/i, () => 'model role reported a blocking outcome'],
];
for (const [pattern, label] of categories) {
  const match = text.match(pattern);
  if (match) {
    process.stderr.write(`local Ollama failure category: ${label(match)}; raw content omitted\n`);
    process.exit(0);
  }
}
process.stderr.write('local Ollama failure category: unclassified before acceptance checkpoint; raw content omitted\n');
NODE
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
    if [[ "$acceptance_mode" == "fake" ]]; then
      echo "$label stderr:" >&2
      sed 's/^/  /' "$stderr" >&2 || true
    else
      report_local_failure "$stderr"
      echo "$label failed; provider stderr was retained only in ephemeral acceptance state" >&2
    fi
    fail "$label command failed"
  fi
  if [[ -s "$stderr" ]]; then
    if [[ "$acceptance_mode" == "fake" ]]; then
      echo "$label stderr:" >&2
      sed 's/^/  /' "$stderr" >&2 || true
    fi
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
    if [[ "$acceptance_mode" == "fake" ]]; then
      sed 's/^/  /' "$output.stderr" >&2 || true
    fi
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
    if [[ "$acceptance_mode" == "fake" ]]; then
      echo "$label stderr:" >&2
      sed 's/^/  /' "$stderr" >&2 || true
    else
      report_local_failure "$stderr"
      echo "$label failed; provider stderr was retained only in ephemeral acceptance state" >&2
    fi
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
  local expected_provider="fake"
  local expected_model="fake-local"

  if [[ "$acceptance_mode" == "ollama" ]]; then
    expected_provider="ollama"
    expected_model="$model"
  fi

  node - "$file" "$command" "$run_id" "$status" "$expected_provider" "$expected_model" <<'NODE'
const fs = require('node:fs');
const [file, command, runId, status, expectedProvider, expectedModel] = process.argv.slice(2);
const report = JSON.parse(fs.readFileSync(file, 'utf8'));
if (report.command !== command || report.run_id !== runId || report.status !== status) {
  throw new Error(`unexpected loop report: ${JSON.stringify(report)}`);
}
if (report.provider !== undefined && (report.provider !== expectedProvider || report.model !== expectedModel)) {
  throw new Error('loop report did not use exact provider authority');
}
NODE
}

require_live_review_checkpoint() {
  local report_file="$1"
  local run_directory="$2"
  local label="$3"
  local result_variable="$4"
  local classification

  if ! classification="$(node - "$report_file" "$run_directory" "$label" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const [reportPath, runDirectoryInput, label] = process.argv.slice(2);
let currentStep = 'unknown';
let kind = 'none';
let outcome = 'no_terminal_provider_exchange';
let classification;
try {
  const report = JSON.parse(fs.readFileSync(reportPath, 'utf8'));
  currentStep = String(report.current_step ?? currentStep);
  if (report.status === 'awaiting_human_review') {
    classification = 'awaiting_human_review';
  } else {
    const runDirectory = fs.realpathSync(runDirectoryInput);
    const run = JSON.parse(fs.readFileSync(path.join(runDirectory, 'run.json'), 'utf8'));
    const last = Array.isArray(run.provider_exchange_records)
      ? run.provider_exchange_records.at(-1)
      : undefined;
    if (last) {
      kind = String(last.kind);
      const safePath = typeof last.path === 'string'
        && !path.posix.isAbsolute(last.path)
        && !last.path.includes('\\')
        && last.path.split('/').every((part) => part && part !== '.' && part !== '..');
      if (safePath && /^[0-9a-f]{64}$/.test(last.digest)) {
        const recordPath = path.resolve(runDirectory, last.path);
        const stat = fs.lstatSync(recordPath);
        if (recordPath.startsWith(`${runDirectory}${path.sep}`)
          && fs.realpathSync(recordPath) === recordPath
          && stat.isFile()
          && !stat.isSymbolicLink()
          && stat.size <= 4 * 1024 * 1024) {
          const bytes = fs.readFileSync(recordPath);
          const observed = crypto.createHash('sha256').update(bytes).digest('hex');
          const record = JSON.parse(bytes.toString('utf8'));
          outcome = String(record.outcome ?? outcome);
          const verified = observed === last.digest
            && record.run_id === last.run_id
            && record.step === last.step
            && record.role === last.role
            && record.step_attempt === last.step_attempt
            && record.exchange_index === last.exchange_index
            && record.kind === last.kind
            && record.context_round === last.context_round
            && record.phase === last.phase;
          if (report.status === 'blocked'
            && currentStep === 'spec_review'
            && run.status === 'blocked'
            && run.current_step === 'spec_review'
            && verified
            && last.step === 'spec_review'
            && last.step_attempt === 1
            && last.exchange_index === 1
            && last.kind === 'initial'
            && last.context_round === undefined
            && last.phase === 'response'
            && outcome === 'request_changes') {
            classification = 'recoverable_spec_review';
          }
        }
      }
    }
  }
} catch (_) {
  // The caller receives only the bounded classification below.
}
if (classification) {
  process.stdout.write(classification);
  process.exit(0);
}
process.stderr.write(`local Ollama ${label} stopped at ${currentStep} with ${kind}/${outcome}; raw provider content omitted\n`);
process.exit(1);
NODE
  )"; then
    fail "$label did not reach a supported review checkpoint"
  fi
  printf -v "$result_variable" '%s' "$classification"
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

render_ollama_eval_config() {
  local destination="$1"
  local mode="$2"
  local control_dir="$3"

  [[ "$mode" == "pass" || "$mode" == "reject" ]] || fail "invalid Ollama fixture mode"
  [[ "$control_dir" =~ ^[A-Za-z0-9._/-]+$ ]] || fail "Ollama control directory is not template-safe"
  sed \
    -e "s|@SEAF_GOLDEN_PATH_MODE@|$mode|g" \
    -e "s|@SEAF_GOLDEN_PATH_CONTROL_DIR@|$control_dir|g" \
    "$ollama_fixture_root/seaf.evals.yaml.in" >"$destination"
}

materialize_ollama_repository() {
  local repository="$1"
  local control_dir="$2"
  local mode="$3"
  local label="$4"
  local init_output="$temp_root/$label-init.json"
  local ticket_output="$temp_root/$label-ticket.json"
  local doctor_output="$temp_root/$label-doctor.json"

  mkdir "$repository" "$control_dir"
  chmod 0700 "$repository" "$control_dir"
  install -m 0644 "$ollama_fixture_root/ollama-acceptance.txt" \
    "$repository/ollama-acceptance.txt"
  install -m 0755 "$ollama_fixture_root/ollama-native-check.sh" \
    "$repository/ollama-native-check.sh"
  run_git -C "$repository" init -q
  run_git -C "$repository" config user.name "SEAF Ollama Golden Path"
  run_git -C "$repository" config user.email "ollama-golden-path@seaf.invalid"

  run_in_repository "$label generic init" "$repository" "$init_output" init --json
  node - "$init_output" <<'NODE'
const fs = require('node:fs');
const report = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const expected = ['seaf.config.json', 'seaf.policy.json', 'seaf.evals.yaml', 'seaf.ticket.yaml', '.seaf/.gitignore'];
if (report.path !== '.' || report.template !== 'generic' || JSON.stringify(report.created) !== JSON.stringify(expected)) {
  throw new Error(`generic init output mismatch: ${JSON.stringify(report)}`);
}
NODE
  install -m 0644 "$ollama_fixture_root/seaf.ticket.yaml" "$repository/seaf.ticket.yaml"
  render_ollama_eval_config "$repository/seaf.evals.yaml" "$mode" "$control_dir"
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
    .seaf/.gitignore ollama-acceptance.txt ollama-native-check.sh \
    seaf.config.json seaf.evals.yaml seaf.policy.json seaf.ticket.yaml
  run_git -C "$repository" commit -q -m "Initialize packaged Ollama fixture"
  [[ -z "$(run_git -C "$repository" status --porcelain=v1 --untracked-files=all)" ]] ||
    fail "$label fixture was not clean after initialization"

  run_in_repository "$label live Ollama doctor" "$repository" "$doctor_output" \
    doctor --provider ollama --model "$model" --base-url "$ollama_base_url" \
    --live-provider --timeout-ms 30000 --json
  node - "$doctor_output" "$model" <<'NODE'
const fs = require('node:fs');
const report = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (report.schema_version !== 1
  || report.ready !== true
  || report.provider !== 'ollama'
  || report.model !== process.argv[3]) {
  throw new Error('live Ollama doctor authority mismatch');
}
if (!Array.isArray(report.checks) || report.checks.length !== 8 || report.checks.some((check) => check.status !== 'passed')) {
  throw new Error('live Ollama doctor did not report exactly eight passing checks');
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

snapshot_reviewer_recovery_authority() {
  local run_directory="$1"
  local output="$2"

  node - "$run_directory" "$output" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const root = fs.realpathSync(process.argv[2]);
const output = process.argv[3];
const run = JSON.parse(fs.readFileSync(path.join(root, 'run.json'), 'utf8'));
const MAX_FILES = 128;
const MAX_TOTAL_BYTES = 32 * 1024 * 1024;
if (run.status !== 'blocked' || run.current_step !== 'spec_review') {
  throw new Error('reviewer recovery snapshot requires exact blocked Spec Review authority');
}
if (!Array.isArray(run.provider_exchange_records) || run.provider_exchange_records.length !== 8) {
  throw new Error('reviewer recovery snapshot requires exactly four initial provider attempts');
}
const digestPattern = /^[0-9a-f]{64}$/;
function safeRelative(relative) {
  return typeof relative === 'string'
    && !path.posix.isAbsolute(relative)
    && !relative.includes('\\')
    && relative.split('/').every((part) => part && part !== '.' && part !== '..');
}
function normalizedSafeRelative(relative) {
  if (!safeRelative(relative)) throw new Error('unsafe immutable attempt-one path');
  const normalized = path.posix.normalize(relative);
  if (normalized !== relative) throw new Error('noncanonical immutable attempt-one path');
  return normalized;
}
const immutablePaths = new Set();
const immutableCache = new Map();
let totalBytes = 0;
function addImmutablePath(relative) {
  const normalized = normalizedSafeRelative(relative);
  immutablePaths.add(normalized);
  if (immutablePaths.size > MAX_FILES) {
    throw new Error('immutable attempt-one inventory exceeds file-count bound');
  }
  return normalized;
}
function readImmutable(relative) {
  const normalized = addImmutablePath(relative);
  const cached = immutableCache.get(normalized);
  if (cached) return cached;
  const absolute = path.resolve(root, normalized);
  if (!absolute.startsWith(`${root}${path.sep}`)) throw new Error('immutable attempt-one path escaped run');
  const stat = fs.lstatSync(absolute);
  if (fs.realpathSync(absolute) !== absolute
    || !stat.isFile()
    || stat.isSymbolicLink()
    || stat.size > 4 * 1024 * 1024) {
    throw new Error('unsafe immutable attempt-one file');
  }
  if (totalBytes + stat.size > MAX_TOTAL_BYTES) {
    throw new Error('immutable attempt-one inventory exceeds aggregate bound');
  }
  const bytes = fs.readFileSync(absolute);
  if (bytes.length !== stat.size) throw new Error('immutable attempt-one file changed while reading');
  totalBytes += bytes.length;
  const entry = {
    path: normalized,
    size: bytes.length,
    sha256: crypto.createHash('sha256').update(bytes).digest('hex'),
    bytes,
  };
  immutableCache.set(normalized, entry);
  return entry;
}
const spec = run.steps?.find((step) => step.name === 'spec_creation');
const review = run.steps?.find((step) => step.name === 'spec_review');
for (const [label, step, status] of [
  ['prior spec', spec, 'completed'],
  ['reviewer artifact', review, 'blocked'],
]) {
  if (!step || step.status !== status || !safeRelative(step.artifact_path) || !digestPattern.test(step.artifact_digest)) {
    throw new Error(`${label} authority is absent`);
  }
  if (readImmutable(step.artifact_path).sha256 !== step.artifact_digest) {
    throw new Error(`${label} artifact digest is not authenticated`);
  }
}
for (const directory of ['prompts', 'responses']) {
  const absoluteDirectory = path.join(root, directory);
  for (const name of fs.readdirSync(absoluteDirectory)) {
    if (!name || name.includes('/') || name.includes('\\')) throw new Error('unsafe immutable entry name');
    addImmutablePath(`${directory}/${name}`);
  }
}
for (const step of run.steps ?? []) {
  if (step.artifact_path) addImmutablePath(step.artifact_path);
}
for (const reference of run.provider_exchange_records) {
  addImmutablePath(reference.path);
  const recordEntry = readImmutable(reference.path);
  if (recordEntry.sha256 !== reference.digest) throw new Error('provider ledger reference digest mismatch');
  const record = JSON.parse(recordEntry.bytes.toString('utf8'));
  addImmutablePath(record.request?.path);
  if (record.response?.path) addImmutablePath(record.response.path);
  if (record.expansion?.path) addImmutablePath(record.expansion.path);
}
if (immutablePaths.size > MAX_FILES) {
  throw new Error('immutable attempt-one inventory exceeds file-count bound');
}
const entries = [...immutablePaths].sort().map((relative) => {
  const { bytes: _, ...entry } = readImmutable(relative);
  return entry;
});
if (entries.length > MAX_FILES || totalBytes > MAX_TOTAL_BYTES) {
  throw new Error('immutable attempt-one inventory exceeds final bounds');
}
const snapshot = {
  provider_ledger_prefix: run.provider_exchange_records,
  prior_spec_artifact_digest: spec.artifact_digest,
  reviewer_artifact_digest: review.artifact_digest,
  immutable_attempt_one: entries,
};
fs.writeFileSync(output, `${JSON.stringify(snapshot)}\n`, { flag: 'wx', mode: 0o600 });
NODE
}

assert_reviewer_recovery_preserved() {
  local run_directory="$1"
  local snapshot="$2"

  node - "$run_directory" "$snapshot" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const root = fs.realpathSync(process.argv[2]);
const expected = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
const run = JSON.parse(fs.readFileSync(path.join(root, 'run.json'), 'utf8'));
const prefix = run.provider_exchange_records?.slice(0, expected.provider_ledger_prefix.length);
if (JSON.stringify(prefix) !== JSON.stringify(expected.provider_ledger_prefix)) {
  throw new Error('original provider ledger is not an unchanged prefix');
}
for (const entry of expected.immutable_attempt_one) {
  const absolute = path.resolve(root, entry.path);
  if (!absolute.startsWith(`${root}${path.sep}`)) throw new Error('immutable attempt-one path escaped run');
  const stat = fs.lstatSync(absolute);
  if (fs.realpathSync(absolute) !== absolute
    || !stat.isFile()
    || stat.isSymbolicLink()
    || stat.size !== entry.size) {
    throw new Error(`immutable attempt-one file changed: ${entry.path}`);
  }
  const observed = crypto.createHash('sha256').update(fs.readFileSync(absolute)).digest('hex');
  if (observed !== entry.sha256) throw new Error(`immutable attempt-one bytes changed: ${entry.path}`);
}
NODE
}

recover_live_spec_review() {
  local repository="$1"
  local runs_root="$2"
  local run_id="$3"
  local label="$4"
  local snapshot="$5"
  local result_output="$6"
  local recovery_variable="$7"
  local run_directory="$runs_root/$run_id"
  local revise_output="$temp_root/$label-spec-revise.json"
  local rerun_output="$result_output"
  local recovery_id
  local rerun_checkpoint

  snapshot_reviewer_recovery_authority "$run_directory" "$snapshot"
  run_in_repository "revise $label from authenticated Spec Review" "$repository" "$revise_output" \
    loop revise --run-id "$run_id" --runs-root "$runs_root" --from-step spec \
    --actor "$operator" --reason "address authenticated packaged Spec Review feedback" --json
  node - "$revise_output" <<'NODE'
const report = JSON.parse(require('node:fs').readFileSync(process.argv[2], 'utf8'));
if (report.command !== 'revise'
  || report.status !== 'pending'
  || report.current_step !== 'spec_creation'
  || report.source_step_attempt !== 1
  || report.next_step_attempt !== 2
  || !Number.isSafeInteger(report.recovery_id)
  || report.recovery_id < 1) {
  throw new Error('Spec Creation revision authority mismatch');
}
NODE
  recovery_id="$(json_value "$revise_output" recovery_id)"
  run_in_repository "rerun $label from authenticated Spec Review" "$repository" "$rerun_output" \
    loop rerun --run-id "$run_id" --runs-root "$runs_root" --recovery "$recovery_id" \
    --ticket seaf.ticket.yaml --base-url "$ollama_base_url" --timeout-ms "$role_timeout_ms" --json
  require_live_review_checkpoint "$rerun_output" "$run_directory" "$label revised run" rerun_checkpoint
  [[ "$rerun_checkpoint" == "awaiting_human_review" ]] ||
    fail "$label revised run stopped before the human review checkpoint"
  assert_loop_report "$rerun_output" rerun "$run_id" awaiting_human_review
  assert_reviewer_recovery_preserved "$run_directory" "$snapshot"
  printf -v "$recovery_variable" '%s' "$recovery_id"
}

assert_clean_live_provider_flow() {
  local run_file="$1"
  local inspect_file="$2"
  local flow="$3"

  node - "$run_file" "$inspect_file" "$flow" <<'NODE'
const fs = require('node:fs');
const run = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const inspect = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
const flow = process.argv[4];
const clean = [
  ['research', 1, 'passed'],
  ['analysis', 1, 'passed'],
  ['spec_creation', 1, 'passed'],
  ['spec_review', 1, 'approve_spec'],
  ['development', 1, 'patch_proposed'],
  ['output_review', 1, 'approve_for_tests'],
];
const recovered = [
  ['research', 1, 'passed'],
  ['analysis', 1, 'passed'],
  ['spec_creation', 1, 'passed'],
  ['spec_review', 1, 'request_changes'],
  ['spec_creation', 2, 'passed'],
  ['spec_review', 2, 'approve_spec'],
  ['development', 1, 'patch_proposed'],
  ['output_review', 1, 'approve_for_tests'],
];
const expected = flow === 'rejection_clean'
  ? clean
  : (flow === 'passing_recovered' || flow === 'rejection_recovered' ? recovered : undefined);
if (!expected) throw new Error(`unsupported live provider flow: ${flow}`);
const records = run.provider_exchange_records;
if (!Array.isArray(records) || records.length !== expected.length * 2) {
  throw new Error(`live provider ledger has an unexpected exact record count: ${records?.length}`);
}
for (let index = 0; index < expected.length; index += 1) {
  const [step, attempt] = expected[index];
  const request = records[index * 2];
  const response = records[index * 2 + 1];
  for (const record of [request, response]) {
    if (record.step !== step
      || record.step_attempt !== attempt
      || record.exchange_index !== 1
      || record.kind !== 'initial'
      || record.context_round !== undefined) {
      throw new Error(`live provider exchange used retry, repair, expansion, or non-initial authority: ${JSON.stringify(record)}`);
    }
  }
  if (request.phase !== 'request' || response.phase !== 'response') {
    throw new Error(`live provider exchange phases are not one request and one response for ${step}/${attempt}`);
  }
}
if (!Array.isArray(inspect.provider_attempts) || inspect.provider_attempts.length !== expected.length) {
  throw new Error('inspection did not retain the exact provider attempt count');
}
for (let index = 0; index < inspect.provider_attempts.length; index += 1) {
  const attempt = inspect.provider_attempts[index];
  const [step, stepAttempt, outcome] = expected[index];
  if (attempt.step !== step || attempt.attempt !== stepAttempt || attempt.exchanges.length !== 2) {
    throw new Error(`unexpected inspected provider attempt: ${JSON.stringify(attempt)}`);
  }
  const [request, response] = attempt.exchanges;
  if (request.kind !== 'initial'
    || response.kind !== 'initial'
    || request.phase !== 'request'
    || response.phase !== 'response'
    || request.exchange_index !== 1
    || response.exchange_index !== 1
    || request.context_round !== undefined
    || response.context_round !== undefined
    || response.outcome !== outcome
    || request.verification !== 'verified'
    || response.verification !== 'verified') {
    throw new Error(`provider attempt is not a clean terminal exchange: ${JSON.stringify(attempt)}`);
  }
}
NODE
}

show_candidate_authority() {
  local label="$1"
  local status_file="$2"
  local inspect_file="$3"
  local run_directory="$4"
  local status
  local reviewed_diff

  node - "$status_file" "$inspect_file" <<'NODE'
const fs = require('node:fs');
const [statusPath, inspectPath] = process.argv.slice(2);
const status = JSON.parse(fs.readFileSync(statusPath, 'utf8'));
const inspect = JSON.parse(fs.readFileSync(inspectPath, 'utf8'));
if (inspect.integrity !== 'verified'
  || inspect.candidate?.verification !== 'verified'
  || inspect.run_id !== status.run_id
  || inspect.status !== status.status) {
  throw new Error('display authority is not bound to verified inspection');
}
NODE
  status="$(json_value "$status_file" status)"
  if [[ "$status" == "awaiting_human_review" ]]; then
    local run_file="$run_directory/run.json"
    assert_exact_ollama_candidate "$run_file" 'M  ollama-acceptance.txt'
    local candidate_path
    candidate_path="$(node -e 'const r=require(process.argv[1]); process.stdout.write(r.candidate_workspace.path)' "$run_file")"
    [[ -d "$candidate_path" && ! -L "$candidate_path" ]] || fail "$label candidate workspace is unsafe"
    reviewed_diff="$(mktemp "$temp_root/reviewed-candidate-diff.XXXXXXXX")" ||
      fail "$label could not create an ephemeral reviewed diff"
    run_git -C "$candidate_path" diff \
      --cached --binary --full-index --no-ext-diff --no-textconv HEAD -- \
      >"$reviewed_diff"
    local observed_digest
    observed_digest="$(sha256_file "$reviewed_diff")"
    [[ "$observed_digest" == "$(json_value "$status_file" candidate_diff_digest)" ]] ||
      fail "$label regenerated candidate diff does not match displayed candidate authority"
  else
    local candidate_relative
    candidate_relative="$(json_value "$status_file" candidate_diff_path)"
    [[ "$candidate_relative" == artifacts/*
      && "$candidate_relative" != *\\*
      && "$candidate_relative" != */../*
      && "$candidate_relative" != ../* ]] || fail "$label candidate diff path is unsafe"
    reviewed_diff="$run_directory/$candidate_relative"
    [[ -f "$reviewed_diff" && ! -L "$reviewed_diff" ]] || fail "$label candidate diff is missing"
    [[ "$(sha256_file "$reviewed_diff")" == "$(json_value "$status_file" candidate_diff_digest)" ]] ||
      fail "$label candidate diff artifact does not match displayed candidate authority"
  fi
  (("$(file_size_bytes "$reviewed_diff")" <= 32768)) || fail "$label candidate diff exceeds display bound"

  node - "$label" "$status_file" "$inspect_file" <<'NODE'
const fs = require('node:fs');
const [label, statusPath, inspectPath] = process.argv.slice(2);
const status = JSON.parse(fs.readFileSync(statusPath, 'utf8'));
const inspect = JSON.parse(fs.readFileSync(inspectPath, 'utf8'));
const shown = {
  run: status.run_id,
  status: status.status,
  candidate_diff_digest: status.candidate_diff_digest,
  eval_report_digest: status.eval_report_digest ?? null,
  target_head: status.target_head,
  integrity: inspect.integrity,
};
process.stdout.write(`==> ${label} authority\n${JSON.stringify(shown, null, 2)}\n`);
NODE
  echo "==> $label inspected candidate diff"
  sed -n '1,400p' "$reviewed_diff"
}

require_typed_confirmation() {
  local label="$1"
  local expected="$2"
  local entered

  [[ -t 0 ]] || fail "$label requires an interactive terminal with human-entered input"
  printf '%s: ' "$label" >&2
  if ! IFS= read -r entered; then
    fail "$label requires interactive typed input; received EOF"
  fi
  [[ "$entered" == "$expected" ]] || fail "$label did not exactly match the displayed authority"
}

assert_exact_ollama_candidate() {
  local run_file="$1"
  local expected_status="$2"

  local candidate_path
  candidate_path="$(node -e 'const r=require(process.argv[1]); process.stdout.write(r.candidate_workspace.path)' "$run_file")"
  [[ -d "$candidate_path" && ! -L "$candidate_path" ]] || fail "Ollama candidate workspace is unsafe"
  [[ "$(run_git -C "$candidate_path" status --porcelain=v1 --untracked-files=all)" == "$expected_status" ]] ||
    fail "Ollama candidate changed a path outside the one-file contract"
  printf 'SEAF packaged Ollama acceptance passed.\n' |
    cmp -s - "$candidate_path/ollama-acceptance.txt" ||
    fail "Ollama candidate target bytes do not match the exact newline-terminated contract"
}

artifact_metrics() {
  local directory="$1"
  local output="$2"

  node - "$directory" "$output" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const root = fs.realpathSync(process.argv[2]);
const entries = [];
let totalBytes = 0;
function walk(directory, relativeDirectory = '', depth = 0) {
  if (depth > 8) throw new Error('evidence inventory exceeds depth bound');
  for (const name of fs.readdirSync(directory).sort()) {
    const relative = relativeDirectory ? `${relativeDirectory}/${name}` : name;
    const absolute = path.join(directory, name);
    const stat = fs.lstatSync(absolute);
    if (stat.isSymbolicLink()) throw new Error(`evidence inventory contains symlink: ${relative}`);
    if (stat.isDirectory()) {
      walk(absolute, relative, depth + 1);
      continue;
    }
    if (!stat.isFile() || stat.size > 4 * 1024 * 1024) throw new Error(`unsafe evidence inventory entry: ${relative}`);
    const bytes = fs.readFileSync(absolute);
    totalBytes += bytes.length;
    if (totalBytes > 32 * 1024 * 1024) throw new Error('evidence inventory exceeds aggregate bound');
    entries.push({ path: relative, mode: stat.mode & 0o7777, size: bytes.length, sha256: crypto.createHash('sha256').update(bytes).digest('hex') });
    if (entries.length > 4096) throw new Error('evidence inventory exceeds file-count bound');
  }
}
walk(root);
const canonical = `${JSON.stringify(entries)}\n`;
const report = {
  file_count: entries.length,
  total_bytes: totalBytes,
  manifest_sha256: crypto.createHash('sha256').update(canonical).digest('hex'),
};
fs.writeFileSync(process.argv[3], `${JSON.stringify(report)}\n`, { flag: 'wx', mode: 0o600 });
NODE
}

sha256_file() {
  node -e 'const c=require("node:crypto"),f=require("node:fs");process.stdout.write(c.createHash("sha256").update(f.readFileSync(process.argv[1])).digest("hex"))' "$1"
}

fixture_manifest_sha256() {
  node - "$ollama_fixture_root" <<'NODE'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const root = fs.realpathSync(process.argv[2]);
const entries = fs.readdirSync(root).sort().map((name) => {
  const absolute = path.join(root, name);
  const stat = fs.lstatSync(absolute);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`unsafe Ollama fixture entry: ${name}`);
  const bytes = fs.readFileSync(absolute);
  return { name, mode: stat.mode & 0o7777, size: bytes.length, sha256: crypto.createHash('sha256').update(bytes).digest('hex') };
});
process.stdout.write(crypto.createHash('sha256').update(`${JSON.stringify(entries)}\n`).digest('hex'));
NODE
}

validate_evidence_status_allowlist() {
  local evidence_file="$1"

  node - "$evidence_file" <<'NODE'
const evidence = JSON.parse(require('node:fs').readFileSync(process.argv[2], 'utf8'));
if (evidence.schema_version !== 2) throw new Error('evidence schema version mismatch');
const digest = /^[0-9a-f]{64}$/;
const allowed = new Set([
  'pending',
  'running',
  'blocked',
  'failed',
  'awaiting_human_review',
  'approved',
  'eval_passed',
  'promoted',
]);
for (const [label, transitions] of [
  ['passing', evidence.passing?.status_transitions],
  ['rejection', evidence.rejection?.status_transitions],
]) {
  if (!Array.isArray(transitions) || transitions.length === 0) {
    throw new Error(`${label} status transitions are absent`);
  }
  for (const status of transitions) {
    if (!allowed.has(status)) throw new Error(`${label} contains invented LoopStatus: ${status}`);
  }
}
if (JSON.stringify(evidence.passing.status_transitions)
    !== JSON.stringify(['blocked', 'pending', 'awaiting_human_review', 'approved', 'eval_passed', 'promoted'])
  || evidence.passing.interruption_observed !== true
  || evidence.passing.provider_attempt_count !== 8
  || evidence.passing.provider_ledger_record_count !== 16
  || evidence.passing.reviewer_recovery?.source_step !== 'spec_creation'
  || evidence.passing.reviewer_recovery?.blocked_step !== 'spec_review'
  || evidence.passing.reviewer_recovery?.source_attempt !== 1
  || evidence.passing.reviewer_recovery?.revised_attempt !== 2
  || !Number.isSafeInteger(evidence.passing.reviewer_recovery?.recovery_id)
  || evidence.passing.reviewer_recovery.recovery_id < 1
  || !Number.isSafeInteger(evidence.passing.evaluation_recovery_id)
  || evidence.passing.evaluation_recovery_id !== evidence.passing.reviewer_recovery.recovery_id + 1
  || !digest.test(evidence.passing.reviewer_recovery?.prior_spec_artifact_digest)
  || !digest.test(evidence.passing.reviewer_recovery?.reviewer_artifact_digest)
  || evidence.passing.reviewer_recovery?.attempt_one_immutable !== true
  || evidence.passing.reviewer_recovery?.ledger_prefix_preserved !== true
  || JSON.stringify(evidence.passing.evaluation_attempts) !== JSON.stringify([1, 2])
  || evidence.passing.source_unchanged_before_promotion !== true
  || evidence.passing.approved_candidate_equals_promoted_source !== true
  || evidence.passing.attempt_one_immutable !== true) {
  throw new Error('passing status/interruption facts do not match executed authority');
}
const cleanRejection = JSON.stringify(['awaiting_human_review', 'approved', 'failed']);
const recoveredRejection = JSON.stringify(['blocked', 'pending', 'awaiting_human_review', 'approved', 'failed']);
const rejectionTransitions = JSON.stringify(evidence.rejection.status_transitions);
const rejectionCountsMatch = rejectionTransitions === cleanRejection
  ? evidence.rejection.provider_attempt_count === 6 && evidence.rejection.provider_ledger_record_count === 12
  : rejectionTransitions === recoveredRejection
    && evidence.rejection.provider_attempt_count === 8
    && evidence.rejection.provider_ledger_record_count === 16;
if (!rejectionCountsMatch
  || evidence.rejection.candidate_lifecycle !== 'cleaned'
  || JSON.stringify(evidence.rejection.evaluation_attempts) !== JSON.stringify([1])
  || evidence.rejection.source_unchanged !== true
  || evidence.rejection.sentinels_unchanged !== true) {
  throw new Error('rejection status/lifecycle facts do not match executed authority');
}
const omissions = evidence.omissions ?? {};
if (Object.values({
  raw_provider_bodies_omitted: omissions.raw_provider_bodies_omitted,
  raw_prompts_omitted: omissions.raw_prompts_omitted,
  raw_responses_omitted: omissions.raw_responses_omitted,
  provider_records_omitted: omissions.provider_records_omitted,
  provider_metadata_omitted: omissions.provider_metadata_omitted,
  absolute_paths_omitted: omissions.absolute_paths_omitted,
  command_output_omitted: omissions.command_output_omitted,
}).some((value) => value !== true)) {
  throw new Error('evidence omission contract mismatch');
}
NODE
}

remove_owned_evidence_temp() {
  [[ -z "$evidence_temp" ]] && return 0
  rm -f -- "$evidence_temp" || return 1
  evidence_temp=""
}

write_ollama_evidence() {
  local passing_metrics="$1"
  local rejection_metrics="$2"
  local archive_sha256="$3"
  local harness_sha256="$4"
  local fixture_sha256="$5"
  local candidate_digest="$6"
  local eval_digest="$7"
  local target_head="$8"
  local rejection_candidate="$9"
  shift 9
  local rejection_eval_digest="$1"
  local rejection_head="$2"
  local passing_recovery_snapshot="$3"
  local provider_recovery_id="$4"
  local evaluation_recovery_id="$5"
  local rejection_recovered="$6"
  local rejection_provider_attempt_count="$7"
  local rejection_provider_ledger_record_count="$8"

  local evidence_parent
  evidence_parent="$(dirname "$evidence_out")"
  evidence_temp="$(mktemp "$evidence_parent/.seaf-m2-07-evidence.XXXXXXXX")" ||
    fail "could not create secure evidence publication temporary file"
  chmod 0600 "$evidence_temp"

  if ! node - "$evidence_temp" "$passing_metrics" "$rejection_metrics" \
    "$ollama_server_version" "$ollama_client_version" "$model" "$ollama_host" "$ollama_base_url" \
    "$source_head" "$archive_sha256" "$harness_sha256" "$fixture_sha256" \
    "$candidate_digest" "$eval_digest" "$target_head" "$rejection_candidate" \
    "$rejection_eval_digest" "$rejection_head" "$passing_recovery_snapshot" \
    "$provider_recovery_id" "$evaluation_recovery_id" "$rejection_recovered" \
    "$rejection_provider_attempt_count" "$rejection_provider_ledger_record_count" \
    "$(uname -s)" "$(uname -m)" <<'NODE'
const fs = require('node:fs');
const [
  temporary, passingMetricsPath, rejectionMetricsPath, serverVersion, clientVersion,
  model, host, apiBaseUrl, sourceCommit, archiveSha256, harnessSha256, fixtureSha256,
  candidateDigest, evalDigest, targetHead, rejectionCandidate, rejectionEvalDigest,
  rejectionHead, passingRecoverySnapshotPath, providerRecoveryId, evaluationRecoveryId,
  rejectionRecovered, rejectionProviderAttemptCount, rejectionProviderLedgerRecordCount, os, arch,
] = process.argv.slice(2);
const digest = /^[0-9a-f]{64}$/;
const commit = /^[0-9a-f]{40,64}$/;
for (const value of [archiveSha256, harnessSha256, fixtureSha256, candidateDigest, evalDigest, rejectionCandidate, rejectionEvalDigest]) {
  if (!digest.test(value)) throw new Error('evidence contains invalid digest authority');
}
for (const value of [sourceCommit, targetHead, rejectionHead]) {
  if (!commit.test(value)) throw new Error('evidence contains invalid Git authority');
}
const reviewerAuthority = JSON.parse(fs.readFileSync(passingRecoverySnapshotPath, 'utf8'));
const providerRecovery = Number(providerRecoveryId);
const evaluationRecovery = Number(evaluationRecoveryId);
if (!digest.test(reviewerAuthority.prior_spec_artifact_digest)
  || !digest.test(reviewerAuthority.reviewer_artifact_digest)
  || !Number.isSafeInteger(providerRecovery)
  || providerRecovery < 1
  || !Number.isSafeInteger(evaluationRecovery)
  || evaluationRecovery < 1
  || evaluationRecovery !== providerRecovery + 1) {
  throw new Error('reviewer recovery evidence authority mismatch');
}
const rejectionWasRecovered = rejectionRecovered === 'true';
const rejectionTransitions = rejectionWasRecovered
  ? ['blocked', 'pending', 'awaiting_human_review', 'approved', 'failed']
  : ['awaiting_human_review', 'approved', 'failed'];
const evidence = {
  schema_version: 2,
  milestone: 'M2-07',
  generated_at: new Date().toISOString(),
  platform: { os, arch },
  ollama: { server_version: serverVersion, client_version: clientVersion, model, host, api_base_url: apiBaseUrl },
  source_commit: sourceCommit,
  packaged_archive_sha256: archiveSha256,
  harness_sha256: harnessSha256,
  fixture_manifest_sha256: fixtureSha256,
  cli_identity: { version: 'seaf 0.1.0', info: 'Self-Evolving Application Framework' },
  passing: {
    status_transitions: ['blocked', 'pending', 'awaiting_human_review', 'approved', 'eval_passed', 'promoted'],
    interruption_observed: true,
    evaluation_attempts: [1, 2],
    evaluation_recovery_id: evaluationRecovery,
    provider_attempt_count: 8,
    provider_ledger_record_count: 16,
    reviewer_recovery: {
      recovery_id: providerRecovery,
      source_step: 'spec_creation',
      blocked_step: 'spec_review',
      source_attempt: 1,
      revised_attempt: 2,
      prior_spec_artifact_digest: reviewerAuthority.prior_spec_artifact_digest,
      reviewer_artifact_digest: reviewerAuthority.reviewer_artifact_digest,
      attempt_one_immutable: true,
      ledger_prefix_preserved: true,
    },
    integrity: 'verified',
    candidate_diff_digest: candidateDigest,
    eval_report_digest: evalDigest,
    target_head: targetHead,
    source_unchanged_before_promotion: true,
    approved_candidate_equals_promoted_source: true,
    attempt_one_immutable: true,
    artifacts: JSON.parse(fs.readFileSync(passingMetricsPath, 'utf8')),
  },
  rejection: {
    status_transitions: rejectionTransitions,
    candidate_lifecycle: 'cleaned',
    evaluation_attempts: [1],
    provider_attempt_count: Number(rejectionProviderAttemptCount),
    provider_ledger_record_count: Number(rejectionProviderLedgerRecordCount),
    integrity: 'verified',
    candidate_diff_digest: rejectionCandidate,
    eval_report_digest: rejectionEvalDigest,
    target_head: rejectionHead,
    source_unchanged: true,
    sentinels_unchanged: true,
    artifacts: JSON.parse(fs.readFileSync(rejectionMetricsPath, 'utf8')),
  },
  omissions: {
    raw_provider_bodies_omitted: true,
    raw_prompts_omitted: true,
    raw_responses_omitted: true,
    provider_records_omitted: true,
    provider_metadata_omitted: true,
    absolute_paths_omitted: true,
    command_output_omitted: true,
  },
};
const bytes = Buffer.from(`${JSON.stringify(evidence, null, 2)}\n`, 'utf8');
if (bytes.length > 32 * 1024) throw new Error('sanitized evidence exceeds 32 KiB');
const descriptor = fs.openSync(temporary, 'r+');
try {
  fs.ftruncateSync(descriptor, 0);
  fs.writeFileSync(descriptor, bytes);
  fs.fsyncSync(descriptor);
} finally {
  fs.closeSync(descriptor);
}
NODE
  then
    remove_owned_evidence_temp || true
    return 1
  fi
  if ! validate_evidence_status_allowlist "$evidence_temp"; then
    remove_owned_evidence_temp || true
    return 1
  fi
  if ! node - "$evidence_temp" "$evidence_out" <<'NODE'
const fs = require('node:fs');
fs.linkSync(process.argv[2], process.argv[3]);
NODE
  then
    remove_owned_evidence_temp || true
    return 1
  fi
  remove_owned_evidence_temp || fail "could not remove evidence publication temporary file"
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

run_ollama_acceptance() {
  local passing_repo="$temp_root/ollama-passing-repo"
  local passing_control="$temp_root/ollama-passing-control"
  local passing_runs="$temp_root/ollama-passing-runs"
  local rejection_repo="$temp_root/ollama-rejection-repo"
  local rejection_control="$temp_root/ollama-rejection-control"
  local rejection_runs="$temp_root/ollama-rejection-runs"

  echo "==> Materialize live Ollama passing project"
  materialize_ollama_repository "$passing_repo" "$passing_control" pass ollama-passing
  local passing_source_before="$temp_root/ollama-passing-source-before.json"
  snapshot_repository "$passing_repo" "$passing_source_before"

  local passing_run_output="$temp_root/ollama-passing-run.json"
  run_in_repository "live Ollama passing loop run" "$passing_repo" "$passing_run_output" \
    loop run --ticket seaf.ticket.yaml --provider ollama --model "$model" \
    --base-url "$ollama_base_url" --timeout-ms "$role_timeout_ms" \
    --runs-root "$passing_runs" --run-id "$ollama_passing_run_id" --json
  local passing_run_dir="$passing_runs/$ollama_passing_run_id"
  local passing_run_file="$passing_run_dir/run.json"
  local passing_checkpoint
  require_live_review_checkpoint "$passing_run_output" \
    "$passing_run_dir" "passing run" passing_checkpoint
  [[ "$passing_checkpoint" == "recoverable_spec_review" ]] ||
    fail "passing run reached first-pass human review without exercising reviewer recovery"
  local passing_recovery_snapshot="$temp_root/ollama-passing-reviewer-recovery.json"
  local passing_recovered_output="$temp_root/ollama-passing-recovered.json"
  local provider_recovery_id
  recover_live_spec_review "$passing_repo" "$passing_runs" "$ollama_passing_run_id" \
    "Ollama passing" "$passing_recovery_snapshot" "$passing_recovered_output" \
    provider_recovery_id
  passing_run_output="$passing_recovered_output"
  local candidate_digest
  candidate_digest="$(json_value "$passing_run_output" candidate_diff_digest)"
  local target_head
  target_head="$(json_value "$passing_run_output" target_head)"
  [[ "$candidate_digest" =~ ^[0-9a-f]{64}$ ]] || fail "Ollama passing candidate digest is invalid"
  [[ "$target_head" =~ ^[0-9a-f]{40,64}$ ]] || fail "Ollama passing target HEAD is invalid"
  local provider_count
  provider_count="$(node -e 'const r=require(process.argv[1]); process.stdout.write(String(r.provider_exchange_records.length))' "$passing_run_file")"
  [[ "$provider_count" == "16" ]] || fail "Ollama passing run did not contain exactly eight provider attempts"

  local passing_awaiting_inspect="$temp_root/ollama-passing-awaiting-inspect.json"
  inspect_run "$passing_repo" "$passing_runs" "$ollama_passing_run_id" awaiting_human_review \
    "$passing_awaiting_inspect"
  assert_clean_live_provider_flow "$passing_run_file" "$passing_awaiting_inspect" passing_recovered
  assert_exact_ollama_candidate "$passing_run_file" 'M  ollama-acceptance.txt'
  snapshot_repository "$passing_repo" "$temp_root/ollama-passing-before-approval-source.json"
  assert_same_snapshot "Ollama source before human approval" "$passing_source_before" \
    "$temp_root/ollama-passing-before-approval-source.json"

  local passing_status_before="$temp_root/ollama-passing-status-before-approval.json"
  run_in_repository "Ollama passing approval status" "$passing_repo" "$passing_status_before" \
    loop status --run-id "$ollama_passing_run_id" --runs-root "$passing_runs" --json
  assert_loop_report "$passing_status_before" status "$ollama_passing_run_id" awaiting_human_review
  show_candidate_authority "Ollama passing approval" "$passing_status_before" \
    "$passing_awaiting_inspect" "$passing_run_dir"
  require_typed_confirmation "Type the passing candidate digest" "$candidate_digest"
  require_typed_confirmation "Type the passing target HEAD" "$target_head"

  local passing_approve_output="$temp_root/ollama-passing-approve.json"
  run_in_repository "exact Ollama passing approval" "$passing_repo" "$passing_approve_output" \
    loop approve --run-id "$ollama_passing_run_id" --runs-root "$passing_runs" \
    --reviewer "$reviewer" --confirm-candidate-diff "$candidate_digest" \
    --confirm-target-head "$target_head" --json
  assert_loop_report "$passing_approve_output" approve "$ollama_passing_run_id" approved
  [[ "$(json_value "$passing_approve_output" testing_ran)" == "false" ]] ||
    fail "Ollama approval unexpectedly ran Testing"
  inspect_run "$passing_repo" "$passing_runs" "$ollama_passing_run_id" approved \
    "$temp_root/ollama-passing-approved-inspect.json"

  echo "==> Interrupt packaged Ollama evaluation and recover with attempt 2"
  local passing_resume_output="$temp_root/ollama-passing-resume-interrupted.json"
  (
    cd "$passing_repo"
    exec "$seaf_binary" loop resume --run-id "$ollama_passing_run_id" \
      --runs-root "$passing_runs" --base-url "$ollama_base_url" \
      --timeout-ms "$role_timeout_ms" --json
  ) >"$passing_resume_output" 2>"$passing_resume_output.stderr" &
  active_resume_pid=$!
  wait_for_file "$passing_control/started" "Ollama fixture evaluation start marker"
  wait_for_file "$passing_control/eval.pid" "Ollama fixture evaluation PID marker"
  active_eval_pid="$(tr -d '\r\n' <"$passing_control/eval.pid")"
  [[ "$active_eval_pid" =~ ^[1-9][0-9]*$ ]] || fail "Ollama fixture evaluation PID is invalid"
  [[ -f "$passing_run_dir/artifacts/07-testing.attempt-001.execution-intent.json" ]] ||
    fail "Ollama evaluation started without durable attempt-1 intent"
  /bin/kill -KILL "$active_resume_pid" || fail "could not interrupt packaged Ollama CLI"
  kill_process_group "$active_eval_pid"
  set +e
  wait "$active_resume_pid"
  local resume_status=$?
  set -e
  active_resume_pid=""
  active_eval_pid=""
  ((resume_status != 0)) || fail "interrupted packaged Ollama CLI unexpectedly succeeded"
  snapshot_repository "$passing_repo" "$temp_root/ollama-passing-after-interruption-source.json"
  assert_same_snapshot "Ollama source after real interruption" "$passing_source_before" \
    "$temp_root/ollama-passing-after-interruption-source.json"
  snapshot_directory "$passing_run_dir" "$temp_root/ollama-attempt-one-before-recovery.json" \
    "artifacts/07-testing.attempt-001"
  inspect_run "$passing_repo" "$passing_runs" "$ollama_passing_run_id" approved \
    "$temp_root/ollama-passing-interrupted-inspect.json" true

  snapshot_directory "$passing_run_dir" "$temp_root/ollama-before-ordinary-resume.json"
  run_in_repository_fails_with \
    "ordinary incomplete Ollama evaluation resume" "$passing_repo" \
    "$temp_root/ollama-ordinary-resume.stdout" \
    "an incomplete Approved evaluation attempt exists; audited recovery is required" \
    loop resume --run-id "$ollama_passing_run_id" --runs-root "$passing_runs" \
    --base-url "$ollama_base_url" --timeout-ms "$role_timeout_ms" --json
  snapshot_directory "$passing_run_dir" "$temp_root/ollama-after-ordinary-resume.json"
  assert_same_snapshot "Ollama run authority after rejected ordinary resume" \
    "$temp_root/ollama-before-ordinary-resume.json" "$temp_root/ollama-after-ordinary-resume.json"

  local passing_revise_output="$temp_root/ollama-passing-revise.json"
  run_in_repository "invalidate incomplete Ollama evaluation" "$passing_repo" "$passing_revise_output" \
    loop revise --run-id "$ollama_passing_run_id" --runs-root "$passing_runs" \
    --from-step testing --eval-recovery invalidate --actor "$operator" \
    --reason "recover packaged Ollama acceptance interruption" --json
  node - "$passing_revise_output" "$provider_recovery_id" <<'NODE'
const report = JSON.parse(require('node:fs').readFileSync(process.argv[2], 'utf8'));
const expected = Number(process.argv[3]) + 1;
if (report.command !== 'revise' || report.recovery_id !== expected || report.invalidated_attempt !== 1 || report.next_evaluation_attempt !== 2) {
  throw new Error('Ollama evaluation invalidation authority mismatch');
}
NODE
  local evaluation_recovery_id
  evaluation_recovery_id="$(json_value "$passing_revise_output" recovery_id)"
  snapshot_directory "$passing_run_dir" "$temp_root/ollama-attempt-one-after-revise.json" \
    "artifacts/07-testing.attempt-001"
  assert_same_snapshot "Ollama evaluation attempt 1 after revise" \
    "$temp_root/ollama-attempt-one-before-recovery.json" "$temp_root/ollama-attempt-one-after-revise.json"
  assert_provider_count "$passing_run_file" "$provider_count"

  : >"$passing_control/release"
  local passing_rerun_output="$temp_root/ollama-passing-rerun.json"
  run_in_repository_with_eval_cleanup_diagnostic \
    "rerun invalidated Ollama evaluation" "$passing_repo" "$passing_rerun_output" \
    loop rerun --run-id "$ollama_passing_run_id" --runs-root "$passing_runs" \
    --recovery "$evaluation_recovery_id" --base-url "$ollama_base_url" \
    --timeout-ms "$role_timeout_ms" --json
  assert_loop_report "$passing_rerun_output" rerun "$ollama_passing_run_id" eval_passed
  assert_provider_count "$passing_run_file" "$provider_count"
  snapshot_directory "$passing_run_dir" "$temp_root/ollama-attempt-one-after-rerun.json" \
    "artifacts/07-testing.attempt-001"
  assert_same_snapshot "Ollama evaluation attempt 1 after rerun" \
    "$temp_root/ollama-attempt-one-before-recovery.json" "$temp_root/ollama-attempt-one-after-rerun.json"
  printf 'packaged Ollama native check passed\n' |
    cmp -s - "$passing_run_dir/artifacts/07-testing.attempt-002.check-001.stdout.log" ||
    fail "recovered Ollama native check stdout mismatch"
  [[ ! -s "$passing_run_dir/artifacts/07-testing.attempt-002.check-001.stderr.log" ]] ||
    fail "recovered Ollama native check wrote stderr"
  snapshot_repository "$passing_repo" "$temp_root/ollama-passing-after-rerun-source.json"
  assert_same_snapshot "Ollama source after evaluation recovery" "$passing_source_before" \
    "$temp_root/ollama-passing-after-rerun-source.json"
  local passing_eval_inspect="$temp_root/ollama-passing-eval-passed-inspect.json"
  inspect_run "$passing_repo" "$passing_runs" "$ollama_passing_run_id" eval_passed \
    "$passing_eval_inspect"
  assert_clean_live_provider_flow "$passing_run_file" "$passing_eval_inspect" passing_recovered

  local passing_status_output="$temp_root/ollama-passing-promotion-status.json"
  run_in_repository "Ollama passing promotion status" "$passing_repo" "$passing_status_output" \
    loop status --run-id "$ollama_passing_run_id" --runs-root "$passing_runs" --json
  assert_loop_report "$passing_status_output" status "$ollama_passing_run_id" eval_passed
  local promotion_candidate
  promotion_candidate="$(json_value "$passing_status_output" candidate_diff_digest)"
  local promotion_eval
  promotion_eval="$(json_value "$passing_status_output" eval_report_digest)"
  local promotion_head
  promotion_head="$(json_value "$passing_status_output" target_head)"
  [[ "$promotion_candidate" == "$candidate_digest" ]] || fail "Ollama promotion candidate drifted"
  [[ "$promotion_eval" =~ ^[0-9a-f]{64}$ ]] || fail "Ollama promotion EvalReport digest is invalid"
  [[ "$promotion_head" == "$target_head" ]] || fail "Ollama promotion target HEAD drifted"
  show_candidate_authority "Ollama passing promotion" "$passing_status_output" \
    "$passing_eval_inspect" "$passing_run_dir"
  require_typed_confirmation "Type the promotion candidate digest" "$promotion_candidate"
  require_typed_confirmation "Type the promotion EvalReport digest" "$promotion_eval"
  require_typed_confirmation "Type the promotion target HEAD" "$promotion_head"

  local passing_promote_output="$temp_root/ollama-passing-promote.json"
  run_in_repository "exact Ollama passing promotion" "$passing_repo" "$passing_promote_output" \
    loop promote --run-id "$ollama_passing_run_id" --runs-root "$passing_runs" \
    --reviewer "$reviewer" --confirm-candidate-diff "$promotion_candidate" \
    --confirm-eval-report "$promotion_eval" --confirm-target-head "$promotion_head" --json
  assert_loop_report "$passing_promote_output" promote "$ollama_passing_run_id" promoted
  [[ "$(run_git -C "$passing_repo" rev-parse HEAD)" == "$target_head" ]] || fail "Ollama promotion changed target HEAD"
  [[ "$(run_git -C "$passing_repo" write-tree)" == "$(node -e 'process.stdout.write(JSON.parse(require("node:fs").readFileSync(process.argv[1],"utf8")).indexTree)' "$passing_source_before")" ]] ||
    fail "Ollama promotion changed target index"
  [[ "$(run_git -C "$passing_repo" status --porcelain=v1 --untracked-files=all)" == ' M ollama-acceptance.txt' ]] ||
    fail "Ollama promotion left changes outside the one permitted target"
  printf 'SEAF packaged Ollama acceptance passed.\n' |
    cmp -s - "$passing_repo/ollama-acceptance.txt" || fail "promoted Ollama source bytes mismatch"
  assert_exact_ollama_candidate "$passing_run_file" 'M  ollama-acceptance.txt'
  local candidate_path
  candidate_path="$(node -e 'const r=require(process.argv[1]); process.stdout.write(r.candidate_workspace.path)' "$passing_run_file")"
  cmp -s "$passing_repo/ollama-acceptance.txt" "$candidate_path/ollama-acceptance.txt" ||
    fail "promoted Ollama source differs from frozen candidate"
  local candidate_diff_path
  candidate_diff_path="$(json_value "$passing_status_output" candidate_diff_path)"
  run_git -C "$candidate_path" diff --cached --binary --full-index --no-ext-diff --no-textconv HEAD -- \
    >"$temp_root/ollama-frozen-candidate.diff"
  cmp -s "$passing_run_dir/$candidate_diff_path" "$temp_root/ollama-frozen-candidate.diff" ||
    fail "frozen Ollama candidate diff does not match approved artifact"
  local passing_promoted_inspect="$temp_root/ollama-passing-promoted-inspect.json"
  inspect_run "$passing_repo" "$passing_runs" "$ollama_passing_run_id" promoted \
    "$passing_promoted_inspect"
  assert_clean_live_provider_flow "$passing_run_file" "$passing_promoted_inspect" passing_recovered
  validate_run_artifacts "$passing_run_dir" "promoted Ollama passing run"

  echo "==> Materialize live Ollama rejection project"
  materialize_ollama_repository "$rejection_repo" "$rejection_control" reject ollama-rejection
  local rejection_source_before="$temp_root/ollama-rejection-source-before.json"
  snapshot_repository "$rejection_repo" "$rejection_source_before"
  local rejection_run_output="$temp_root/ollama-rejection-run.json"
  run_in_repository "live Ollama rejection loop run" "$rejection_repo" "$rejection_run_output" \
    loop run --ticket seaf.ticket.yaml --provider ollama --model "$model" \
    --base-url "$ollama_base_url" --timeout-ms "$role_timeout_ms" \
    --runs-root "$rejection_runs" --run-id "$ollama_rejection_run_id" --json
  local rejection_run_dir="$rejection_runs/$ollama_rejection_run_id"
  local rejection_run_file="$rejection_run_dir/run.json"
  local rejection_checkpoint
  local rejection_recovered=false
  local rejection_flow=rejection_clean
  require_live_review_checkpoint "$rejection_run_output" \
    "$rejection_run_dir" "rejection run" rejection_checkpoint
  if [[ "$rejection_checkpoint" == "recoverable_spec_review" ]]; then
    local rejection_recovery_snapshot="$temp_root/ollama-rejection-reviewer-recovery.json"
    local rejection_recovered_output="$temp_root/ollama-rejection-recovered.json"
    local rejection_provider_recovery_id
    recover_live_spec_review "$rejection_repo" "$rejection_runs" "$ollama_rejection_run_id" \
      "Ollama rejection" "$rejection_recovery_snapshot" "$rejection_recovered_output" \
      rejection_provider_recovery_id
    rejection_run_output="$rejection_recovered_output"
    rejection_recovered=true
    rejection_flow=rejection_recovered
  else
    [[ "$rejection_checkpoint" == "awaiting_human_review" ]] ||
      fail "rejection run reached an unsupported review checkpoint"
    assert_loop_report "$rejection_run_output" run "$ollama_rejection_run_id" awaiting_human_review
  fi
  local rejection_candidate
  rejection_candidate="$(json_value "$rejection_run_output" candidate_diff_digest)"
  local rejection_head
  rejection_head="$(json_value "$rejection_run_output" target_head)"
  local rejection_provider_attempt_count=6
  local rejection_provider_ledger_record_count=12
  if [[ "$rejection_recovered" == "true" ]]; then
    rejection_provider_attempt_count=8
    rejection_provider_ledger_record_count=16
  fi
  assert_provider_count "$rejection_run_file" "$rejection_provider_ledger_record_count"
  local rejection_awaiting_inspect="$temp_root/ollama-rejection-awaiting-inspect.json"
  inspect_run "$rejection_repo" "$rejection_runs" "$ollama_rejection_run_id" awaiting_human_review \
    "$rejection_awaiting_inspect"
  assert_clean_live_provider_flow "$rejection_run_file" "$rejection_awaiting_inspect" "$rejection_flow"
  assert_exact_ollama_candidate "$rejection_run_file" 'M  ollama-acceptance.txt'
  local rejection_status="$temp_root/ollama-rejection-status.json"
  run_in_repository "Ollama rejection approval status" "$rejection_repo" "$rejection_status" \
    loop status --run-id "$ollama_rejection_run_id" --runs-root "$rejection_runs" --json
  assert_loop_report "$rejection_status" status "$ollama_rejection_run_id" awaiting_human_review
  show_candidate_authority "Ollama rejection approval" "$rejection_status" \
    "$rejection_awaiting_inspect" "$rejection_run_dir"
  require_typed_confirmation "Type the rejection candidate digest" "$rejection_candidate"
  require_typed_confirmation "Type the rejection target HEAD" "$rejection_head"

  local rejection_approve_output="$temp_root/ollama-rejection-approve.json"
  run_in_repository "exact Ollama rejection approval" "$rejection_repo" "$rejection_approve_output" \
    loop approve --run-id "$ollama_rejection_run_id" --runs-root "$rejection_runs" \
    --reviewer "$reviewer" --confirm-candidate-diff "$rejection_candidate" \
    --confirm-target-head "$rejection_head" --json
  assert_loop_report "$rejection_approve_output" approve "$ollama_rejection_run_id" approved
  snapshot_repository "$rejection_repo" "$temp_root/ollama-rejection-after-approval-clean.json"
  assert_same_snapshot "Ollama rejection source after approval" "$rejection_source_before" \
    "$temp_root/ollama-rejection-after-approval-clean.json"
  printf 'packaged rejection preservation sentinel\n' >"$rejection_repo/rejection-untracked-sentinel.txt"
  chmod 0640 "$rejection_repo/rejection-untracked-sentinel.txt"
  (umask 000; ln -s rejection-untracked-sentinel.txt "$rejection_repo/rejection-untracked-sentinel.link")
  local rejection_source_with_sentinels="$temp_root/ollama-rejection-source-with-sentinels.json"
  snapshot_repository "$rejection_repo" "$rejection_source_with_sentinels"
  assert_rejection_sentinels "$rejection_source_with_sentinels"

  local rejection_resume_output="$temp_root/ollama-rejection-resume.json"
  run_in_repository_with_eval_cleanup_diagnostic \
    "deterministic Ollama rejecting evaluation" "$rejection_repo" "$rejection_resume_output" \
    loop resume --run-id "$ollama_rejection_run_id" --runs-root "$rejection_runs" \
    --base-url "$ollama_base_url" --timeout-ms "$role_timeout_ms" --json
  assert_loop_report "$rejection_resume_output" resume "$ollama_rejection_run_id" failed
  local rejection_report="$rejection_run_dir/artifacts/08-eval-report.attempt-001.json"
  node - "$rejection_report" "$rejection_run_dir" <<'NODE'
const fs = require('node:fs');
const path = require('node:path');
const report = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const runDirectory = fs.realpathSync(process.argv[3]);
if (report.passed !== false || report.decision !== 'reject' || report.checks?.length !== 1) {
  throw new Error('rejecting Ollama EvalReport authority mismatch');
}
const check = report.checks[0];
if (check.status !== 'failed' || check.name !== 'packaged_ollama_native_check' || check.summary !== 'command exited with code 24') {
  throw new Error('rejecting Ollama native check summary mismatch');
}
function bytes(relative) {
  if (typeof relative !== 'string' || path.posix.isAbsolute(relative) || relative.includes('\\') || relative.split('/').some((part) => !part || part === '.' || part === '..')) {
    throw new Error('unsafe rejecting Ollama log reference');
  }
  const absolute = path.resolve(runDirectory, relative);
  if (fs.realpathSync(absolute) !== absolute || !absolute.startsWith(`${runDirectory}${path.sep}`)) throw new Error('rejecting Ollama log escaped run');
  return fs.readFileSync(absolute);
}
if (bytes(check.stdout_path).length !== 0) throw new Error('rejecting Ollama native check wrote stdout');
const expected = Buffer.from('ollama golden-path check: deterministic rejection requested\n', 'utf8');
if (!bytes(check.stderr_path).equals(expected)) throw new Error('rejecting Ollama native check stderr mismatch');
NODE
  snapshot_repository "$rejection_repo" "$temp_root/ollama-rejection-after-failure.json"
  assert_same_snapshot "Ollama rejection source after failure" "$rejection_source_with_sentinels" \
    "$temp_root/ollama-rejection-after-failure.json"
  local rejection_failed_inspect="$temp_root/ollama-rejection-failed-inspect.json"
  inspect_run "$rejection_repo" "$rejection_runs" "$ollama_rejection_run_id" failed \
    "$rejection_failed_inspect"
  assert_clean_live_provider_flow "$rejection_run_file" "$rejection_failed_inspect" "$rejection_flow"
  validate_run_artifacts "$rejection_run_dir" "failed Ollama rejection run"

  local rejection_cleanup_output="$temp_root/ollama-rejection-cleanup.json"
  run_in_repository "Ollama rejection candidate cleanup" "$rejection_repo" "$rejection_cleanup_output" \
    loop cleanup --run-id "$ollama_rejection_run_id" --runs-root "$rejection_runs" --json
  node - "$rejection_cleanup_output" <<'NODE'
const report = JSON.parse(require('node:fs').readFileSync(process.argv[2], 'utf8'));
if (report.command !== 'cleanup' || report.status !== 'failed' || report.candidate_lifecycle !== 'cleaned') {
  throw new Error('Ollama rejection cleanup authority mismatch');
}
NODE
  snapshot_repository "$rejection_repo" "$temp_root/ollama-rejection-after-cleanup.json"
  assert_same_snapshot "Ollama rejection source after cleanup" "$rejection_source_with_sentinels" \
    "$temp_root/ollama-rejection-after-cleanup.json"
  local rejection_cleaned_inspect="$temp_root/ollama-rejection-cleaned-inspect.json"
  inspect_run "$rejection_repo" "$rejection_runs" "$ollama_rejection_run_id" failed \
    "$rejection_cleaned_inspect"
  assert_clean_live_provider_flow "$rejection_run_file" "$rejection_cleaned_inspect" "$rejection_flow"
  validate_run_artifacts "$rejection_run_dir" "cleaned Ollama rejection run"

  [[ "$(run_git -C "$repo_root" rev-parse HEAD)" == "$source_head" ]] || fail "SEAF source HEAD changed during Ollama acceptance"
  [[ "$(run_git -C "$repo_root" write-tree)" == "$source_index" ]] || fail "SEAF source index changed during Ollama acceptance"
  snapshot_repository "$repo_root" "$temp_root/ollama-source-final.json"
  assert_same_snapshot "SEAF source repository" "$source_before_snapshot" "$temp_root/ollama-source-final.json"

  local passing_metrics="$temp_root/ollama-passing-metrics.json"
  local rejection_metrics="$temp_root/ollama-rejection-metrics.json"
  artifact_metrics "$passing_run_dir" "$passing_metrics"
  artifact_metrics "$rejection_run_dir" "$rejection_metrics"
  write_ollama_evidence "$passing_metrics" "$rejection_metrics" \
    "$(sha256_file "$archive_path")" "$(sha256_file "$repo_root/scripts/test-packaged-external-golden-path.sh")" \
    "$(fixture_manifest_sha256)" "$candidate_digest" "$promotion_eval" "$target_head" \
    "$rejection_candidate" "$(sha256_file "$rejection_report")" "$rejection_head" \
    "$passing_recovery_snapshot" "$provider_recovery_id" "$evaluation_recovery_id" \
    "$rejection_recovered" "$rejection_provider_attempt_count" \
    "$rejection_provider_ledger_record_count"
  echo "Packaged local Ollama golden path passed; sanitized evidence published."
}

for command in cargo git gzip node sed tar; do
  require_command "$command"
done
if [[ "$acceptance_mode" == "ollama" ]]; then
  for command in awk ollama; do
    require_command "$command"
  done
  ollama_command="$(command -v ollama)"
  [[ "$ollama_command" == /* && -x "$ollama_command" ]] || fail "Ollama command must resolve to an absolute executable"

  evidence_parent="$(dirname "$evidence_out")"
  [[ -d "$evidence_parent" ]] || fail "--evidence-out parent is missing"
  evidence_parent="$(cd "$evidence_parent" && pwd -P)"
  case "$evidence_parent/" in
    "$repo_root/"*) fail "--evidence-out must be outside the SEAF source repository" ;;
  esac
  evidence_name="$(basename "$evidence_out")"
  [[ -n "$evidence_name" && "$evidence_name" != "." && "$evidence_name" != ".." ]] ||
    fail "--evidence-out filename is invalid"
  evidence_out="$evidence_parent/$evidence_name"
  [[ ! -e "$evidence_out" && ! -L "$evidence_out" ]] || fail "--evidence-out must not already exist"
fi
[[ -x "$build_release_script" ]] || fail "release artifact builder is missing or not executable"
for relative in project.txt golden-path-check.sh seaf.ticket.yaml seaf.evals.yaml.in; do
  [[ -f "$fixture_root/$relative" && ! -L "$fixture_root/$relative" ]] ||
    fail "required fixture file is missing or unsafe: fixtures/packaged-external-golden-path/$relative"
done
[[ -x "$fixture_root/golden-path-check.sh" ]] || fail "fixture-native check is not executable"
if [[ "$acceptance_mode" == "ollama" ]]; then
  for relative in ollama-acceptance.txt ollama-native-check.sh seaf.ticket.yaml seaf.evals.yaml.in; do
    [[ -f "$ollama_fixture_root/$relative" && ! -L "$ollama_fixture_root/$relative" ]] ||
      fail "required Ollama fixture file is missing or unsafe: fixtures/packaged-external-golden-path/ollama/$relative"
  done
  [[ -x "$ollama_fixture_root/ollama-native-check.sh" ]] || fail "Ollama fixture-native check is not executable"
fi
validate_ollama_review_fixture

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

if [[ "$acceptance_mode" == "ollama" ]]; then
  OLLAMA_HOST="$ollama_host" "$ollama_command" --version \
    >"$temp_root/ollama-version.txt" 2>&1 || fail "Ollama version preflight failed"
  read -r ollama_server_version ollama_client_version < <(
    node - "$temp_root/ollama-version.txt" <<'NODE'
const text = require('node:fs').readFileSync(process.argv[2], 'utf8');
const server = text.match(/ollama version is ([0-9]+(?:\.[0-9]+)+)/i)?.[1];
const client = text.match(/client version is ([0-9]+(?:\.[0-9]+)+)/i)?.[1] ?? server;
if (!server || !client) process.exit(1);
process.stdout.write(`${server} ${client}\n`);
NODE
  ) || fail "Ollama version preflight did not expose parseable client/server facts"
  OLLAMA_HOST="$ollama_host" "$ollama_command" list \
    >"$temp_root/ollama-list.txt" 2>"$temp_root/ollama-list.stderr" ||
    fail "Ollama model inventory preflight failed"
  [[ ! -s "$temp_root/ollama-list.stderr" ]] || fail "Ollama model inventory wrote unexpected stderr"
  awk -v wanted="$model" 'NR > 1 && $1 == wanted { found += 1 } END { exit(found == 1 ? 0 : 1) }' \
    "$temp_root/ollama-list.txt" || fail "exact Ollama model is not installed once: $model"
fi

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

if [[ "$acceptance_mode" == "ollama" ]]; then
  model_check_output="$temp_root/ollama-model-check.json"
  if ! "$seaf_binary" model check --provider ollama --model "$model" \
    --base-url "$ollama_base_url" --timeout-ms 30000 --json \
    >"$model_check_output" 2>"$model_check_output.stderr"; then
    fail "packaged Ollama model check failed; stderr retained only in ephemeral acceptance state"
  fi
  [[ ! -s "$model_check_output.stderr" ]] || fail "packaged Ollama model check wrote unexpected stderr"
  node - "$model_check_output" "$model" "$ollama_base_url" <<'NODE'
const report = JSON.parse(require('node:fs').readFileSync(process.argv[2], 'utf8'));
if (report.provider !== 'ollama'
  || report.model !== process.argv[3]
  || report.base_url !== process.argv[4]
  || report.ok !== true
  || report.status !== 'passed'
  || report.error_kind !== null) {
  throw new Error('packaged Ollama model check authority mismatch');
}
NODE
  run_ollama_acceptance
  exit 0
fi

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
