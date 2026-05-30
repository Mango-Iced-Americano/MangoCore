#!/usr/bin/env python3
"""
CI score gate analyzer.

Re-parses QEMU serial output for both arches, checks:
  - basic-musl / basic-glibc / busybox-musl / busybox-glibc: score == all
  - ltp-musl / ltp-glibc: score > 0
  - No panic-like patterns in the last 500 lines of serial output

Exits 0 if all gates pass, 1 otherwise.
Outputs ci-summary.md → GITHUB_STEP_SUMMARY for in-CI display.
"""

import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TESTRESULT = ROOT / "testresult"
JUDGE = ROOT / "judge"

ARCHES = {
    "rv64": TESTRESULT / "output-rv.txt",
    "la64": TESTRESULT / "output-la.txt",
}

FULL_SCORE_GROUPS = [
    "basic-musl",
    "basic-glibc",
    "busybox-musl",
    "busybox-glibc",
]

LTP_GROUPS = [
    "ltp-musl",
    "ltp-glibc",
]

PANIC_RE = re.compile(
    r"(?:"
    # Group 1: Rust panic!() → kernel halts (print by #[panic_handler])
    r"\[kernel\] panicked at|"
    # Group 2: Heap allocation failure → kernel halts (print by #[alloc_error_handler])
    r"=== HEAP ALLOCATION FAILED\s*\(FATAL\)\s*===|"
    # Group 3: trap_from_kernel → kernel halts (loongarch64 kernel-space trap)
    r"a trap .+ from kernel! bad addr\s*=|"
    r"Unhandled Trap Cause|"
    # Group 4: OOM killer → process killed (kernel survives but test fails)
    r"\[OOM killer\]|"
    r"\[oom_handler\]|"
    r"\[frame_alloc\] oom recovery failed|"
    r"\[heap_alloc\] alloc failed|"
    r"\[do_oom\]|"
    r"\[force_swap\]|"
    # Group 5: User process killed by signal (indicates crash)
    r"\[kernel\] SIGILL in application|"
    r"\[kernel\] SIGSEGV in application|"
    r"\[sys_sigreturn\] invalid .+ send SIGSEGV|"
    r"Send SIGSEGV|"
    # Group 6: Unrecoverable page fault
    r"\[do_page_fault\] addr:.+, result: bad addr|"
    r"\[trap\] trigger SIGILL/FPU from exception|"
    r"\[trap\] trigger SIGSEGV from address error|"
    # Group 7: Trace dump + shutdown (test framework interrupted)
    r"\[trace\] Shutting down|"
    # Group 8: LLVM/compiler unwinding failures
    r"fatal runtime error:|stack overflow|"
    # Catch-all: internal unwrap/expect panics (debug builds)
    r"called `Option::unwrap\(\)` on a `None` value|"
    r"called `Result::unwrap\(\)` on an `Err` value"
    r")",
    re.IGNORECASE,
)


def group_score(data):
    """Compute (score, all) for a judge group result."""
    if isinstance(data, list):
        score = sum(float(x.get("score", x.get("pass", 0))) for x in data)
        total = sum(int(x.get("all", x.get("total", 1))) for x in data)
        return score, total
    if isinstance(data, dict):
        return float(data.get("score", data.get("pass", 0))), int(data.get("all", 0))
    return 0.0, 0


def parse_full_json(judge_stdout):
    marker = "Full JSON:"
    if marker not in judge_stdout:
        raise RuntimeError("judge output has no 'Full JSON:' section")
    raw = judge_stdout.split(marker, 1)[1].strip()
    return json.loads(raw)


def run_judge(log_path):
    proc = subprocess.run(
        [sys.executable, str(JUDGE / "run_parse.py"), str(log_path), str(JUDGE)],
        cwd=ROOT,
        text=True,
        capture_output=True,
        timeout=180,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"judge failed rc={proc.returncode}\nSTDOUT:\n{proc.stdout}\nSTDERR:\n{proc.stderr}"
        )
    return parse_full_json(proc.stdout)


def scan_panic(log_path):
    text = log_path.read_text(encoding="utf-8", errors="ignore")
    tail = "\n".join(text.splitlines()[-500:])
    return PANIC_RE.search(tail)


def main():
    TESTRESULT.mkdir(exist_ok=True)
    failures = []
    summary_lines = ["# MangoCore CI — Full QEMU Test Summary", ""]

    for arch, log_path in ARCHES.items():
        summary_lines.append(f"## {arch}")
        summary_lines.append("")

        if not log_path.exists() or log_path.stat().st_size == 0:
            msg = f"{arch}: missing or empty serial output `{log_path}`"
            failures.append(msg)
            summary_lines.append(f"- **FAIL**: {msg}")
            summary_lines.append("")
            continue

        panic_match = scan_panic(log_path)
        if panic_match:
            msg = f"{arch}: panic-like pattern in serial tail: `{panic_match.group(0)}`"
            failures.append(msg)
            summary_lines.append(f"- **FAIL**: {msg}")
            summary_lines.append("")
        else:
            summary_lines.append("- Panic check: PASS")
            summary_lines.append("")

        try:
            result = run_judge(log_path)
        except Exception as e:
            msg = f"{arch}: judge parse failed: {e}"
            failures.append(msg)
            summary_lines.append(f"- **FAIL**: {msg}")
            summary_lines.append("")
            continue

        json_path = TESTRESULT / f"ci-{arch}.json"
        json_path.write_text(json.dumps(result, indent=2, ensure_ascii=False), encoding="utf-8")

        summary_lines.append("| Group | Score | All | % | Gate |")
        summary_lines.append("|---|---:|---:|---:|---|")

        for group in FULL_SCORE_GROUPS:
            score, total = group_score(result.get(group, {}))
            pct = f"{score / total * 100:.0f}%" if total > 0 else "N/A"
            ok = total > 0 and score == total
            gate = "PASS" if ok else "FAIL"
            summary_lines.append(f"| {group} | {int(score)} | {total} | {pct} | {gate} |")
            if not ok:
                failures.append(f"{arch}: {group} must be full score, got {int(score)}/{total}")

        for group in LTP_GROUPS:
            score, total = group_score(result.get(group, {}))
            pct = f"{score / total * 100:.0f}%" if total > 0 else "N/A"
            ok = total > 0 and score > 0
            gate = "PASS" if ok else "FAIL"
            summary_lines.append(f"| {group} | {int(score)} | {total} | {pct} | {gate} |")
            if not ok:
                failures.append(f"{arch}: {group} must have score > 0, got {int(score)}/{total}")

        summary_lines.append("")

    summary = "\n".join(summary_lines) + "\n"
    (TESTRESULT / "ci-summary.md").write_text(summary, encoding="utf-8")

    gh_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if gh_summary:
        Path(gh_summary).write_text(summary, encoding="utf-8")

    print(summary)

    if failures:
        for f in failures:
            print(f"::error::{f}")
        sys.exit(1)

    print("All CI score gates passed.")
    sys.exit(0)


if __name__ == "__main__":
    main()
