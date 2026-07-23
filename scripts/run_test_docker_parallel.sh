#!/usr/bin/env bash

set -euo pipefail

printf '%s\n' 'ERROR: run_test_docker_parallel.sh is deprecated; run python3 scripts/run_full_test.py --serial inside Docker instead.' >&2
exit 64
