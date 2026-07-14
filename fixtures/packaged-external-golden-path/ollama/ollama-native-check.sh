#!/bin/sh

set -eu

candidate_file="ollama-acceptance.txt"
expected_content="SEAF packaged Ollama acceptance passed."

if [ ! -f "$candidate_file" ] || [ -L "$candidate_file" ]; then
  echo "ollama golden-path check: exact target is missing or unsafe" >&2
  exit 20
fi
if [ "$(cat "$candidate_file")" != "$expected_content" ]; then
  echo "ollama golden-path check: exact target bytes are incorrect" >&2
  exit 21
fi
if [ "$(wc -l <"$candidate_file" | tr -d ' ')" != "1" ]; then
  echo "ollama golden-path check: exact trailing newline contract failed" >&2
  exit 21
fi

case "${SEAF_GOLDEN_PATH_MODE:-}" in
  pass)
    control_dir="${SEAF_GOLDEN_PATH_CONTROL_DIR:-}"
    if [ -z "$control_dir" ] || [ ! -d "$control_dir" ] || [ -L "$control_dir" ]; then
      echo "ollama golden-path check: interruption control directory is missing or unsafe" >&2
      exit 22
    fi
    umask 077
    printf '%s\n' "$$" >"$control_dir/eval.pid"
    : >"$control_dir/started"
    wait_count=0
    while [ ! -f "$control_dir/release" ]; do
      wait_count=$((wait_count + 1))
      if [ "$wait_count" -ge 2400 ]; then
        echo "ollama golden-path check: timed out waiting for the bounded release marker" >&2
        exit 23
      fi
      /bin/sleep 0.05
    done
    printf '%s\n' "packaged Ollama native check passed"
    ;;
  reject)
    echo "ollama golden-path check: deterministic rejection requested" >&2
    exit 24
    ;;
  *)
    echo "ollama golden-path check: unsupported deterministic mode" >&2
    exit 25
    ;;
esac
