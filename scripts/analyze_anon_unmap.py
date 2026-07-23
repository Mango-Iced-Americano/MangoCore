#!/usr/bin/env python3
"""Build focused anonymous-VMA release reports from a kernel_perf run."""

from __future__ import annotations

import argparse
import csv
import json
import re
from pathlib import Path


DIAG_RE = re.compile(r"CPYTHON_DIAG_JSON (\{[^\r\n]+\})")
COUNTER_PREFIX = "anon_unmap."


def counter(record: dict[str, object], name: str) -> int:
    deltas = record.get("counter_delta", {})
    if not isinstance(deltas, dict):
        return 0
    return int(deltas.get(COUNTER_PREFIX + name, 0))


def read_diag(run_dir: Path, record: dict[str, object]) -> dict[str, object]:
    candidates = [Path(str(record.get("log", "")))]
    if candidates[0].name:
        candidates.append(run_dir / "raw" / candidates[0].name)
    for path in candidates:
        if not path.is_file():
            continue
        match = DIAG_RE.search(path.read_text(encoding="utf-8", errors="replace"))
        if match:
            return json.loads(match.group(1))
    return {}


def sample_event(record: dict[str, object]) -> dict[str, object]:
    events = record.get("benchmark_events", [])
    if not isinstance(events, list):
        return {}
    for event in events:
        if isinstance(event, dict) and event.get("type") == "sample":
            return event
    return {}


def write_csv(path: Path, rows: list[dict[str, object]]) -> None:
    if not rows:
        return
    with path.open("w", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--clock-hz", type=int, default=100_000_000)
    args = parser.parse_args()

    run_dir = args.run_dir.resolve()
    records_path = run_dir / "records.jsonl"
    records = [json.loads(line) for line in records_path.read_text().splitlines() if line]
    reports = run_dir / "reports"
    reports.mkdir(exist_ok=True)

    synthetic: list[dict[str, object]] = []
    python_rows: list[dict[str, object]] = []
    for record in records:
        test = str(record.get("test", ""))
        if test.startswith("anon_mmap_release_"):
            diag = read_diag(run_dir, record)
            pages = int(diag.get("pages", 0))
            expected = pages * (pages + 1) // 2
            observed = counter(record, "anon_unmap_retain_scan_steps_total")
            synthetic.append(
                {
                    "test": test,
                    "sample_role": "warmup" if test.endswith("_warm") else "formal",
                    "size_mib": int(diag.get("size_mib", 0)),
                    "pages": pages,
                    "elapsed_ms": round(int(diag.get("elapsed_ns", 0)) / 1_000_000, 6),
                    "unmap_ticks_ms": round(
                        counter(record, "anon_unmap_ticks_total") * 1000 / args.clock_hz, 6
                    ),
                    "largest_unmap_ms": round(
                        counter(record, "anon_unmap_ticks_max") * 1000 / args.clock_hz, 6
                    ),
                    "calls": counter(record, "anon_unmap_calls_total"),
                    "resident_pages": counter(record, "anon_unmap_resident_pages_total"),
                    "expected_primary_scan_steps": expected,
                    "observed_scan_steps": observed,
                    "auxiliary_scan_steps": observed - expected,
                    "errors": counter(record, "anon_unmap_errors_total"),
                }
            )
        elif test.startswith("cpython_bench_"):
            sample = sample_event(record)
            if not sample:
                continue
            elapsed = float(sample.get("elapsed_seconds", 0.0))
            benchmark = str(sample.get("benchmark", test.removeprefix("cpython_bench_")))
            sys_seconds = float(sample.get("sys_seconds", 0.0))
            unmap_seconds = counter(record, "anon_unmap_ticks_total") / args.clock_hz
            sys_share: object = ""
            if benchmark != "bm_fork" and sys_seconds:
                sys_share = round(100 * unmap_seconds / sys_seconds, 6)
            python_rows.append(
                {
                    "benchmark": benchmark,
                    "elapsed_seconds": round(elapsed, 9),
                    "user_seconds": round(float(sample.get("user_seconds", 0.0)), 9),
                    "sys_seconds": round(sys_seconds, 9),
                    "anon_unmap_seconds": round(unmap_seconds, 9),
                    "anon_unmap_share_percent": round(
                        100 * unmap_seconds / elapsed if elapsed else 0.0, 6
                    ),
                    "anon_unmap_sys_share_percent": sys_share,
                    "calls": counter(record, "anon_unmap_calls_total"),
                    "requested_pages": counter(record, "anon_unmap_requested_pages_total"),
                    "resident_pages": counter(record, "anon_unmap_resident_pages_total"),
                    "largest_active_pages": counter(record, "anon_unmap_active_before_max"),
                    "retain_scan_steps": counter(record, "anon_unmap_retain_scan_steps_total"),
                    "largest_unmap_seconds": round(
                        counter(record, "anon_unmap_ticks_max") / args.clock_hz, 9
                    ),
                    "pages_le_16": counter(record, "anon_unmap_pages_le_16"),
                    "pages_le_256": counter(record, "anon_unmap_pages_le_256"),
                    "pages_le_4096": counter(record, "anon_unmap_pages_le_4096"),
                    "pages_gt_4096": counter(record, "anon_unmap_pages_gt_4096"),
                    "errors": counter(record, "anon_unmap_errors_total"),
                }
            )

    synthetic.sort(key=lambda row: (int(row["size_mib"]), row["sample_role"] != "warmup"))
    python_rows.sort(key=lambda row: float(row["anon_unmap_share_percent"]), reverse=True)
    write_csv(reports / "anon_unmap_synthetic.csv", synthetic)
    write_csv(reports / "anon_unmap_python.csv", python_rows)

    formal = [row for row in synthetic if row["sample_role"] == "formal"]
    lines = [
        "# Anonymous VMA release quantification",
        "",
        f"Clock: {args.clock_hz} Hz. Counter time is measured inside `Vma::unmap`.",
        "",
        "## Resident anonymous mapping microbenchmark",
        "",
        "| MiB | pages | close ms | unmap ms | largest ms | scans | expected primary | extra |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for row in formal:
        lines.append(
            "| {size_mib} | {pages} | {elapsed_ms:.3f} | {unmap_ticks_ms:.3f} | "
            "{largest_unmap_ms:.3f} | {observed_scan_steps} | "
            "{expected_primary_scan_steps} | {auxiliary_scan_steps} |".format(**row)
        )
    lines.extend(
        [
            "",
            "## Strict-aligned CPython workloads",
            "",
            "| benchmark | elapsed s | anonymous unmap s | body share | sys share | calls | resident pages | max active pages | scans | max call s |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for row in python_rows:
        sys_share = row["anon_unmap_sys_share_percent"]
        sys_share_text = "n/a" if sys_share == "" else f"{float(sys_share):.3f}%"
        lines.append(
            "| {benchmark} | {elapsed_seconds:.3f} | {anon_unmap_seconds:.6f} | "
            "{anon_unmap_share_percent:.3f}% | {sys_share} | {calls} | {resident_pages} | "
            "{largest_active_pages} | {retain_scan_steps} | {largest_unmap_seconds:.6f} |".format(
                sys_share=sys_share_text, **row
            )
        )
    lines.extend(
        [
            "",
            "The share is diagnostic attribution, not an optimized-runtime speedup prediction.",
            "`bm_fork` sys share is n/a because its sample rusage is parent-only while kernel counters include children.",
            "Warmup and setup are excluded by the target-side reset/on/off window.",
            "",
        ]
    )
    (reports / "anon_unmap_quantification.md").write_text(
        "\n".join(lines), encoding="utf-8"
    )
    print(reports / "anon_unmap_quantification.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
