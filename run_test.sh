#!/bin/sh
# Deprecated — use the canonical serial runner instead.
printf '%s\n' 'ERROR: run_test.sh is deprecated; run python3 scripts/run_full_test.py --serial inside Docker instead.' >&2
exit 64
