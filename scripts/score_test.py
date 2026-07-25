#!/usr/bin/env python3
# /// script
# requires-python = ">=3.8"
# ///
"""Score the 11 required libc-paired test groups from one QEMU serial log.

The log is parsed only through ``judge/run_parse.py`` so CI uses the same
START/END group boundaries and judge definitions as the competition harness.
Basic and busybox require a 100% rate for both libc variants.  Each remaining
group/libc variant requires at least 90%; the reported score is the equally
weighted mean of all 22 variant rates, expressed on a 0--100 scale.

Usage: python3 scripts/score_test.py --arch rv64 --log path/to/qemu.log \
    --output testresult/score-rv64.json
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Final

GROUPS: Final[tuple[str, ...]] = (
    "basic",
    "busybox",
    "lua",
    "libctest",
    "iozone",
    "libcbench",
    "lmbench",
    "iperf",
    "netperf",
    "cyclictest",
    "ltp",
)
LIBCS: Final[tuple[str, ...]] = ("musl", "glibc")
STRICT_GROUPS: Final[frozenset[str]] = frozenset({"basic", "busybox"})
TOLERANT_MINIMUM_RATE: Final[float] = 0.90
JSON_MARKER: Final[str] = "Full JSON:"


class ScoreInputError(RuntimeError):
    """Raised when a QEMU log cannot produce a complete judge result."""


@dataclass(frozen=True)
class Counts:
    """One judge group's normalized pass and total-test counts."""

    passed: int
    total: int

    @property
    def failed(self) -> int:
        """Return failures after accounting for every declared test."""
        return self.total - self.passed

    @property
    def rate(self) -> float:
        """Return zero for missing groups so they cannot pass a CI gate."""
        return self.passed / self.total if self.total else 0.0


def _number(value, field: str) -> float:
    """Parse one non-negative numeric value emitted by a judge."""
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        raise ScoreInputError(f"judge field {field!r} must be a non-negative number")
    return float(value)


def _counts_from_entry(entry: Mapping[str, object]) -> Counts:
    """Normalize the dict or per-test-list schemas emitted by judge scripts."""
    raw_pass = entry.get("pass", entry.get("score", 0))
    raw_total = entry.get("all", entry.get("total", 0))
    passed = _number(raw_pass, "pass")
    total = _number(raw_total, "all")
    if not passed.is_integer() or not total.is_integer() or passed > total:
        raise ScoreInputError("judge pass/all values must be consistent integer counts")
    return Counts(int(passed), int(total))


def _counts_from_group(value: object) -> Counts:
    """Convert one official judge payload into pass/fail counts."""
    if isinstance(value, Mapping):
        return _counts_from_entry(value)
    if not isinstance(value, Sequence) or isinstance(value, (str, bytes, bytearray)):
        raise ScoreInputError("judge group must be an object or a list of per-test results")

    passed = 0
    total = 0
    for entry in value:
        if not isinstance(entry, Mapping):
            raise ScoreInputError("judge result list contains a non-object entry")
        test_total = _number(entry.get("all", entry.get("total", 1)), "all")
        test_score = _number(entry.get("pass", entry.get("score", 0)), "pass")
        if not test_total.is_integer() or test_total <= 0:
            continue  # skip malformed entries (e.g. missing test data)
        total += int(test_total)
        passed += int(test_total) if test_score > 0 else 0
    return Counts(passed, total)


def score_groups(results: Mapping[str, object]) -> tuple[dict[str, dict[str, dict[str, int]]], float, bool]:
    """Build the required JSON group matrix and evaluate every score gate."""
    groups: dict[str, dict[str, dict[str, int]]] = {}
    rates: list[float] = []
    passed = True

    for group in GROUPS:
        libc_counts: dict[str, dict[str, int]] = {}
        for libc in LIBCS:
            key = f"{group}-{libc}"
            counts = _counts_from_group(results.get(key, {"pass": 0, "all": 0}))
            libc_counts[libc] = {"pass": counts.passed, "fail": counts.failed}
            if counts.total > 0:
                rates.append(counts.rate)
            if group in STRICT_GROUPS:
                passed = passed and counts.total > 0 and counts.failed == 0
            else:
                passed = passed and counts.total > 0 and counts.rate >= TOLERANT_MINIMUM_RATE
        groups[group] = libc_counts

    return groups, round(100 * sum(rates) / len(rates), 2) if rates else 0.0, passed


def _parse_judge_output(stdout: str) -> dict[str, object]:
    """Extract the official parser's trailing JSON object."""
    if JSON_MARKER not in stdout:
        raise ScoreInputError("judge output omitted the Full JSON marker")
    parsed = json.loads(stdout.split(JSON_MARKER, 1)[1].strip())
    if not isinstance(parsed, dict):
        raise ScoreInputError("judge JSON root must be an object")
    return parsed


def parse_log(log_path: Path, judge_dir: Path) -> dict[str, object]:
    """Run the canonical judge parser for one raw QEMU serial log."""
    if not log_path.is_file() or log_path.stat().st_size == 0:
        raise ScoreInputError(f"QEMU log is missing or empty: {log_path}")
    completed = subprocess.run(
        [sys.executable, str(judge_dir / "run_parse.py"), str(log_path), str(judge_dir)],
        capture_output=True,
        check=False,
        text=True,
        timeout=180,
    )
    if completed.returncode != 0:
        raise ScoreInputError(f"judge exited {completed.returncode}: {completed.stderr.strip()}")
    return _parse_judge_output(completed.stdout)


def _arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=("rv64", "la64"), required=True)
    parser.add_argument("--log", type=Path, required=True, help="raw QEMU serial log")
    parser.add_argument("--output", type=Path, required=True, help="structured score JSON output")
    parser.add_argument("--judge-dir", type=Path, default=Path("judge"))
    parser.add_argument("--table", action="store_true", help="print human-readable table instead of JSON")
    return parser.parse_args()


def _print_table(arch: str, groups: dict, score: float, passed: bool) -> None:
    """Print a human-readable score table."""
    print(f"\n{'=' * 72}")
    print(f"  {arch.upper()} Competition Score")
    print(f"{'=' * 72}")
    print(f"  {'Group':<18} {'musl':<18} {'glibc':<18}")
    print(f"  {'-' * 54}")
    for group, variants in sorted(groups.items()):
        musl = variants.get("musl", {"pass": 0, "fail": 0})
        glibc = variants.get("glibc", {"pass": 0, "fail": 0})
        m_str = f"{musl['pass']}/{musl['pass'] + musl['fail']}"
        g_str = f"{glibc['pass']}/{glibc['pass'] + glibc['fail']}"
        print(f"  {group:<18} {m_str:<18} {g_str:<18}")
    print(f"{'=' * 72}")


def main() -> int:
    """Parse, score, persist, and gate a single architecture's QEMU result."""
    args = _arguments()
    try:
        groups, score, passed = score_groups(parse_log(args.log, args.judge_dir))
    except (OSError, ScoreInputError, subprocess.TimeoutExpired, json.JSONDecodeError) as error:
        print(f"::error::{error}", file=sys.stderr)
        return 2

    payload = {"arch": args.arch, "groups": groups, "score": score, "passed": passed}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    if args.table:
        _print_table(args.arch, groups, score, passed)
    else:
        print(json.dumps(payload, indent=2))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
