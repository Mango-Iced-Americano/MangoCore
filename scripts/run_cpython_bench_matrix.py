#!/usr/bin/env python3
"""Run the complete CPython benchmark matrix through the board serial harness."""

from __future__ import annotations

import argparse
import shlex
import subprocess
import sys
from pathlib import Path

from kernel_perf import SERIAL_COMMAND_LIMIT, serial_work_line_size


ROOT = Path(__file__).resolve().parents[1]
KERNEL_PERF = ROOT / "scripts" / "kernel_perf.py"
BENCHMARKS = (
    "bm_bytesio",
    "bm_chaos",
    "bm_decimal",
    "bm_dict",
    "bm_fileio",
    "bm_float",
    "bm_fork",
    "bm_hash",
    "bm_json_loads",
    "bm_list",
    "bm_nbody",
    "bm_pidigits",
    "bm_regex",
    "bm_richards",
    "bm_sort",
    "bm_spectral_norm",
    "bm_string",
    "bm_thread",
)
MEMORY_IO = {
    "bm_bytesio",
    "bm_decimal",
    "bm_dict",
    "bm_fileio",
    "bm_list",
    "bm_sort",
    "bm_string",
}
CORE = {"bm_fork", "bm_thread"}
TARGET_SCRIPT_TIMEOUT = 1800


def profile_for(benchmark: str) -> str:
    if benchmark in MEMORY_IO:
        return "memory_io"
    if benchmark in CORE:
        return "core"
    return "network_runtime"


def build_command(args: argparse.Namespace, benchmark: str) -> list[str]:
    if args.target_script:
        board_workload = "%s %s %s" % (
            shlex.quote(args.target_script),
            shlex.quote(benchmark),
            shlex.quote(args.target_run_tag),
        )
    else:
        suite = shlex.quote(args.suite_root)
        environment = [
            "CPYTHON_BENCH_WARMUPS=%d" % args.warmups,
            "CPYTHON_BENCH_RUNS=%d" % args.runs,
            "CPYTHON_BENCH_TIMEOUT=%d" % args.module_timeout,
        ]
        if args.runtime_root:
            environment.append("CPYTHON_ROOT=%s" % shlex.quote(args.runtime_root))
        if args.work_base:
            environment.append("CPYTHON_BENCH_WORK_BASE=%s" % shlex.quote(args.work_base))
        else:
            environment.append("CPYTHON_BENCH_REQUIRE_SCRATCH=1")
        if args.trap_only or args.stats_profile:
            environment.append("CPYTHON_BENCH_TARGET_STATS=1")
        if args.target_results_root:
            target_jsonl = "%s/%s.jsonl" % (args.target_results_root.rstrip("/"), benchmark)
            environment.append("CPYTHON_BENCH_JSONL=%s" % shlex.quote(target_jsonl))
        board_workload = " ".join(
            (
                "cd %s &&" % suite,
                *environment,
                "/bin/sh ./cpython_benchmark.sh",
                shlex.quote(benchmark),
            )
        )
    test_name = "cpython_bench_" + benchmark.removeprefix("bm_")
    wrapped_bytes = serial_work_line_size(board_workload, test_name, args.iteration)
    if wrapped_bytes > SERIAL_COMMAND_LIMIT:
        raise SystemExit(
            "wrapped board command exceeds %d bytes for %s: %d; use --target-script"
            % (SERIAL_COMMAND_LIMIT, benchmark, wrapped_bytes)
        )
    command = [
        sys.executable,
        str(KERNEL_PERF),
        "board",
        "--run-dir",
        str(args.run_dir),
        "--serial",
        args.serial,
        "--baud",
        str(args.baud),
        "--test",
        test_name,
        "--iteration",
        str(args.iteration),
        "--build-mode",
        args.build_mode,
        "--cache-state",
        args.cache_state,
        "--timeout",
        str(args.capture_timeout),
        "--char-delay",
        str(args.char_delay),
        "--command",
        board_workload,
    ]
    if args.build_mode == "diag_on":
        command.extend(
            (
                "--profile",
                "core" if args.trap_only else args.stats_profile or profile_for(benchmark),
            )
        )
    if args.quiet:
        command.append("--quiet")
    return command


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--serial", default="/dev/cu.wchusbserial120")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument(
        "--suite-root",
        default="/persist/pyperf/s",
        help="benchmark-only directory on the P4 ext4 filesystem",
    )
    parser.add_argument(
        "--runtime-root",
        default="/persist/python-runtime/current",
        help="side-loaded CPython runtime root on P4 ext4, exported as CPYTHON_ROOT",
    )
    parser.add_argument(
        "--work-base",
        help="explicit writable workload directory, for example /persist/pyperf/w on ext4",
    )
    parser.add_argument(
        "--target-results-root",
        help="board-side ext4 directory for one JSONL file per benchmark",
    )
    parser.add_argument(
        "--target-script",
        help="short board-local launcher used to stay below the serial line limit",
    )
    parser.add_argument(
        "--target-run-tag",
        help="safe P4 result namespace passed to --target-script",
    )
    parser.add_argument(
        "--build-mode",
        choices=("production", "diag_off", "diag_on"),
        required=True,
    )
    parser.add_argument("--benchmark", action="append", choices=BENCHMARKS)
    parser.add_argument("--iteration", type=int, default=1)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--module-timeout", type=int, default=900)
    parser.add_argument("--capture-timeout", type=float, default=1000.0)
    parser.add_argument("--char-delay", type=float, default=0.005)
    parser.add_argument("--cache-state", default="warm-runtime")
    parser.add_argument(
        "--trap-only",
        action="store_true",
        help=(
            "measure the benchmark body with CPYTHON_BENCH_TARGET_STATS=1 and "
            "force the perf_diag core profile"
        ),
    )
    parser.add_argument(
        "--stats-profile",
        choices=("core", "memory_io", "network_runtime"),
        help="measure only the benchmark body with the selected perf_diag profile",
    )
    parser.add_argument("--keep-going", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()

    if args.warmups < 0 or args.runs <= 0:
        parser.error("warmups must be >= 0 and runs must be > 0")
    if args.capture_timeout <= args.module_timeout:
        parser.error("capture timeout must exceed the board-side module timeout")
    if args.trap_only and args.build_mode != "diag_on":
        parser.error("--trap-only requires --build-mode diag_on")
    if args.stats_profile and args.build_mode != "diag_on":
        parser.error("--stats-profile requires --build-mode diag_on")
    if args.trap_only and args.stats_profile:
        parser.error("--trap-only and --stats-profile are mutually exclusive")
    if (args.trap_only or args.stats_profile) and (args.warmups, args.runs) != (1, 1):
        parser.error("diagnostic profiles require exactly one warmup plus one measured run")
    if bool(args.target_script) != bool(args.target_run_tag):
        parser.error("--target-script and --target-run-tag must be provided together")
    if args.target_script and not (args.trap_only or args.stats_profile):
        parser.error("the strict target script requires --trap-only or --stats-profile")
    if args.target_script and args.capture_timeout <= TARGET_SCRIPT_TIMEOUT:
        parser.error(
            "target-script mode requires --capture-timeout greater than %d seconds"
            % TARGET_SCRIPT_TIMEOUT
        )
    if not args.run_dir.is_dir() and not args.dry_run:
        parser.error("run directory does not exist: %s" % args.run_dir)

    selected = args.benchmark or list(BENCHMARKS)
    failures = 0
    for benchmark in selected:
        command = build_command(args, benchmark)
        print("+ " + shlex.join(command), flush=True)
        if args.dry_run:
            continue
        result = subprocess.run(command, cwd=ROOT, check=False)
        if result.returncode:
            failures += 1
            if not args.keep_going:
                break
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
