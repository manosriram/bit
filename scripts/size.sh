#!/bin/sh
# Release binary size limit for the current milestone (ARCHITECTURE.md §13).
set -eu
max=$((5 * 1024 * 1024))
n=$(wc -c < target/release/bit)
echo "bit $n/$max bytes"
[ "$n" -le "$max" ]
