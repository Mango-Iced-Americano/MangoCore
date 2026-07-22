# T5 — Canonical full-test runner (Docker evidence)

- **Date:** 2026-07-22
- **Container:** `c238e449081e4c68d07d193d7e7e7c357406503eaab22305cc763a4d9c2e1161`
- **Mount:** `/home/pxy/projects/MangoCore-cleanup -> /app`
- **Revision before documentation:** `d28fae7a-dirty`

## Static verification

Executed inside `os-dev` (no QEMU was launched):

```sh
python3 -m py_compile scripts/run_full_test.py scripts/full_test/*.py
sh -n scripts/test-qemu-command-matrix.sh scripts/test-image-role-contract.sh scripts/run_test_docker_parallel.sh
sh scripts/test-qemu-command-matrix.sh
sh scripts/test-image-role-contract.sh
python3 scripts/run_full_test.py --dry-run --serial
if make docker-test-parallel; then exit 1; fi
if bash scripts/run_test_docker_parallel.sh; then exit 1; fi
```

Results:

- command matrix passed for RV64 and LA64: normal has exactly `x0+x1`; regression has zero drives;
  all seven failure fixtures exit nonzero.
- image-role contract passed.
- `--dry-run --serial` printed normal, competition, derived-competition, development, debug,
  regression, and ktest commands for both architectures without executing QEMU.
- legacy Docker parallel entry points printed their deprecation diagnostic and exited 64 before
  Docker setup, a build, or QEMU execution.
