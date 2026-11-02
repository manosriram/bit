#!/bin/sh
# Hard LOC budgets from ARCHITECTURE.md §1. Counts non-blank, non-comment lines in src/.
set -eu
fail=0
for b in cmd:300 agent:900 llm:600 tools:800 tui:900; do
  c=${b%%:*} max=${b#*:}
  n=$(find "crates/$c/src" -type f -exec cat {} + | grep -cvE '^[[:space:]]*(//.*)?$' || true)
  echo "$c $n/$max"
  [ "$n" -le "$max" ] || fail=1
done
exit $fail
