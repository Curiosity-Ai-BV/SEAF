#!/bin/sh

set -eu

candidate_file="examples/local-loop/evals/fake-provider-smoke.txt"
expected_content="provider-backed smoke"

if [ ! -f "$candidate_file" ] || [ -L "$candidate_file" ]; then
  echo "golden-path check: fake-provider candidate file is missing or unsafe" >&2
  exit 20
fi
if [ "$(cat "$candidate_file")" != "$expected_content" ]; then
  echo "golden-path check: fake-provider candidate content is incorrect" >&2
  exit 21
fi

case "${SEAF_GOLDEN_PATH_MODE:-}" in
  pass)
    control_dir="${SEAF_GOLDEN_PATH_CONTROL_DIR:-}"
    if [ -z "$control_dir" ] || [ ! -d "$control_dir" ] || [ -L "$control_dir" ]; then
      echo "golden-path check: interruption control directory is missing or unsafe" >&2
      exit 22
    fi
    umask 077
    printf '%s\n' "$$" >"$control_dir/eval.pid"
    : >"$control_dir/started"
    wait_count=0
    while [ ! -f "$control_dir/release" ]; do
      wait_count=$((wait_count + 1))
      if [ "$wait_count" -ge 1200 ]; then
        echo "golden-path check: timed out waiting for the bounded release marker" >&2
        exit 23
      fi
      /bin/sleep 0.05
    done
    printf '%s\n' "packaged external native check passed"
    ;;
  reject)
    echo "golden-path check: deterministic rejection requested" >&2
    exit 24
    ;;
  *)
    echo "golden-path check: unsupported deterministic mode" >&2
    exit 25
    ;;
esac
