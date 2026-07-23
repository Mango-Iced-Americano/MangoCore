#!/usr/bin/env python3
"""Deterministic, serial-friendly CPython benchmark runner for MangoCore.

Each invocation can run one or more benchmark modules, but the board wrapper
starts a fresh CPython process for every module.  Imports and garbage
collection are outside the timed region.  Every measured sample is emitted as
one JSON object so the host harness can retain raw values instead of relying on
a best-case score.
"""

import argparse
import gc
import hashlib
import importlib
import json
import math
import os
import statistics
import sys
import time
import traceback

try:
    import resource
except ImportError:  # pragma: no cover - supported by the MangoCore runtime
    resource = None


BENCH_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(BENCH_DIR)
if REPO_ROOT not in sys.path:
    sys.path.insert(0, REPO_ROOT)

SOURCE_REPOSITORY = "Mango-Iced-Americano/cpython-tests"
SOURCE_REVISION = "c50669c2b59a7d6d979fb12aea42c1b508ed3765"
EVENT_PREFIX = "CPYTHON_BENCH_JSON "
DEFAULT_WARMUPS = 1
DEFAULT_RUNS = 1
PYPERFORMANCE_REVISION = "216cbeb5f828b8ee5864f9bb52f3563d2d1a4846"
TARGET_STATS = os.environ.get("CPYTHON_BENCH_TARGET_STATS") == "1"
TARGET_STATS_ROOT = "/sys/kernel/stats"


def set_target_stats(enabled, reset=False):
    """Gate perf_diag counters around the measured benchmark body only."""
    if not TARGET_STATS:
        return
    if not enabled:
        with open(os.path.join(TARGET_STATS_ROOT, "stats_on"), "w", encoding="ascii") as control:
            control.write("0\n")
        return
    if reset:
        with open(os.path.join(TARGET_STATS_ROOT, "reset"), "w", encoding="ascii") as control:
            control.write("1\n")
    with open(os.path.join(TARGET_STATS_ROOT, "stats_on"), "w", encoding="ascii") as control:
        control.write("1\n")


def positive_int(value):
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("value must be positive")
    return number


def nonnegative_int(value):
    number = int(value)
    if number < 0:
        raise argparse.ArgumentTypeError("value must be non-negative")
    return number


def find_benchmarks():
    return sorted(
        name[:-3]
        for name in os.listdir(BENCH_DIR)
        if name.startswith("bm_") and name.endswith(".py")
    )


def suite_sha256():
    digest = hashlib.sha256()
    names = ["bench_runner.py"] + [name + ".py" for name in find_benchmarks()]
    for name in names:
        digest.update(name.encode("utf-8") + b"\0")
        with open(os.path.join(BENCH_DIR, name), "rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
    return digest.hexdigest()


def append_event(path, line):
    if not path:
        return
    directory = os.path.dirname(os.path.abspath(path))
    os.makedirs(directory, exist_ok=True)
    with open(path, "a", encoding="utf-8") as output:
        output.write(line + "\n")


def emit(event, jsonl_path=None):
    event = {"schema": 1, **event}
    line = json.dumps(event, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
    print(EVENT_PREFIX + line, flush=True)
    append_event(jsonl_path, line)


def rusage_snapshot():
    if resource is None:
        return None
    try:
        usage = resource.getrusage(resource.RUSAGE_SELF)
    except (AttributeError, OSError):
        return None
    return (usage.ru_utime, usage.ru_stime, usage.ru_maxrss)


def finite_number(value):
    return isinstance(value, (int, float)) and math.isfinite(float(value))


def split_result(result):
    """Separate optional phase timings from a deterministic result token."""
    if isinstance(result, dict):
        metrics = {str(key): value for key, value in result.items() if finite_number(value)}
        return metrics, None
    rendered = repr(result)
    if len(rendered) > 160:
        rendered = rendered[:157] + "..."
    return None, rendered


def run_benchmark(module_name, warmups, runs, jsonl_path):
    try:
        module = importlib.import_module("bench." + module_name)
        benchmark = getattr(module, "benchmark", None)
        if benchmark is None:
            raise RuntimeError("benchmark() is missing")

        # The host harness enables stats before launching Python.  Disable them
        # after imports so interpreter startup, module import and warmups do not
        # contaminate the workload counters.
        set_target_stats(False)

        for warmup in range(1, warmups + 1):
            gc.collect()
            benchmark()
            emit(
                {
                    "type": "warmup",
                    "benchmark": module_name,
                    "warmup": warmup,
                },
                jsonl_path,
            )

        elapsed_values = []
        result_tokens = []
        for sample in range(1, runs + 1):
            gc.collect()
            usage_before = rusage_snapshot()
            set_target_stats(True, reset=True)
            try:
                started_ns = time.perf_counter_ns()
                result = benchmark()
                elapsed_ns = time.perf_counter_ns() - started_ns
                usage_after = rusage_snapshot()
            finally:
                set_target_stats(False)
            elapsed_values.append(elapsed_ns)
            metrics, result_token = split_result(result)
            if result_token is not None:
                result_tokens.append(result_token)

            event = {
                "type": "sample",
                "benchmark": module_name,
                "sample": sample,
                "elapsed_ns": elapsed_ns,
                "elapsed_seconds": elapsed_ns / 1_000_000_000.0,
                "result": result_token,
            }
            if metrics is not None:
                event["phase_metrics"] = metrics
            if usage_before is not None and usage_after is not None:
                event.update(
                    {
                        "user_seconds": usage_after[0] - usage_before[0],
                        "sys_seconds": usage_after[1] - usage_before[1],
                        "max_rss": usage_after[2],
                    }
                )
            emit(event, jsonl_path)

        if result_tokens and len(set(result_tokens)) != 1:
            raise RuntimeError("benchmark returned inconsistent result tokens")

        mean_ns = statistics.fmean(elapsed_values)
        cv_percent = (
            statistics.pstdev(elapsed_values) / mean_ns * 100.0
            if len(elapsed_values) > 1 and mean_ns
            else 0.0
        )
        emit(
            {
                "type": "summary",
                "benchmark": module_name,
                "samples": len(elapsed_values),
                "warmups": warmups,
                "median_ns": int(statistics.median(elapsed_values)),
                "min_ns": min(elapsed_values),
                "max_ns": max(elapsed_values),
                "mean_ns": int(mean_ns),
                "cv_percent": cv_percent,
                "status": "pass",
            },
            jsonl_path,
        )
        return True
    except Exception as exc:
        emit(
            {
                "type": "error",
                "benchmark": module_name,
                "status": "fail",
                "error_type": type(exc).__name__,
                "error": str(exc),
                "traceback": traceback.format_exc(limit=8),
            },
            jsonl_path,
        )
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("benchmarks", nargs="*", help="bm_* module names; default: all")
    parser.add_argument("--list", action="store_true", help="list benchmark module names")
    parser.add_argument(
        "--warmups",
        type=nonnegative_int,
        default=nonnegative_int(os.environ.get("CPYTHON_BENCH_WARMUPS", DEFAULT_WARMUPS)),
    )
    parser.add_argument(
        "--runs",
        type=positive_int,
        default=positive_int(os.environ.get("CPYTHON_BENCH_RUNS", DEFAULT_RUNS)),
    )
    parser.add_argument("--jsonl", default=os.environ.get("CPYTHON_BENCH_JSONL"))
    args = parser.parse_args()

    available = find_benchmarks()
    if args.list:
        for name in available:
            print(name)
        return 0

    selected = args.benchmarks or available
    unknown = [name for name in selected if name not in available]
    if unknown:
        parser.error("unknown benchmark(s): " + ", ".join(unknown))

    clock = time.get_clock_info("perf_counter")
    uname = os.uname() if hasattr(os, "uname") else None
    emit(
        {
            "type": "environment",
            "source_repository": SOURCE_REPOSITORY,
            "source_revision": SOURCE_REVISION,
            "pyperformance_revision": PYPERFORMANCE_REVISION,
            "suite_sha256": suite_sha256(),
            "python_version": sys.version.replace("\n", " "),
            "python_executable": sys.executable,
            "machine": uname.machine if uname else "unknown",
            "perf_counter_resolution_seconds": clock.resolution,
            "perf_counter_monotonic": clock.monotonic,
            "warmups": args.warmups,
            "runs": args.runs,
            "tmpdir": os.environ.get("TMPDIR", ""),
            "pycache_prefix": os.environ.get("PYTHONPYCACHEPREFIX", ""),
            "target_stats": TARGET_STATS,
        },
        args.jsonl,
    )

    failures = 0
    for module_name in selected:
        emit({"type": "start", "benchmark": module_name}, args.jsonl)
        if not run_benchmark(module_name, args.warmups, args.runs, args.jsonl):
            failures += 1
    emit(
        {
            "type": "suite_summary",
            "benchmarks": len(selected),
            "passed": len(selected) - failures,
            "failed": failures,
            "status": "pass" if failures == 0 else "fail",
        },
        args.jsonl,
    )
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
