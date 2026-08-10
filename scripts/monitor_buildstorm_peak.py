#!/usr/bin/env python3
"""Stop one diagnostic BuildStorm QEMU after its useful sampling window.

The monitor is deliberately tied to an explicit PID, kernel and overlay.  It
never discovers or signals arbitrary QEMU processes, which lets performance
diagnostics coexist with a teammate's long-running instance.
"""

from __future__ import annotations

import argparse
import os
import re
import signal
import time
from dataclasses import dataclass
from pathlib import Path


EXPECTED_SCHEMA = 6


@dataclass(frozen=True)
class Snapshot:
    sample: int
    active: int
    runnable: int
    run_ticks: int
    clock_hz: int
    busy_cores: float | None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--log", required=True, type=Path)
    parser.add_argument("--pid-file", required=True, type=Path)
    parser.add_argument("--status", required=True, type=Path)
    parser.add_argument("--expected-kernel", required=True)
    parser.add_argument("--expected-overlay", required=True)
    parser.add_argument("--period-seconds", type=float, default=5.0)
    parser.add_argument("--timed-timeout-seconds", type=float, default=900.0)
    parser.add_argument("--boot-timeout-seconds", type=float, default=1800.0)
    parser.add_argument("--post-peak-min-samples", type=int, default=36)
    parser.add_argument("--post-peak-target-samples", type=int, default=60)
    return parser.parse_args()


def append_status(path: Path, message: str) -> None:
    stamp = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    with path.open("a", encoding="utf-8") as status:
        status.write(f"{stamp} {message}\n")
        status.flush()


def field(block: str, name: str) -> int | None:
    match = re.search(rf"(?m)^{re.escape(name)}=(\d+)\s*$", block)
    return int(match.group(1)) if match else None


def complete_blocks(text: str) -> list[tuple[int, str]]:
    pattern = re.compile(
        r"BUILDSTORM_STATS_BEGIN sample=(\d+)(?: heavy=[01])?\r?\n(.*?)"
        r"BUILDSTORM_STATS_END sample=\1(?:\r?\n|$)",
        re.DOTALL,
    )
    return [(int(match.group(1)), match.group(2)) for match in pattern.finditer(text)]


def read_pid(path: Path) -> int | None:
    try:
        value = int(path.read_text(encoding="ascii").strip())
    except (FileNotFoundError, ValueError):
        return None
    return value if value > 1 else None


def process_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def terminate_owned_qemu(args: argparse.Namespace, pid: int, reason: str) -> bool:
    try:
        cmdline = Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\0", b" ").decode(
            "utf-8", errors="replace"
        )
    except FileNotFoundError:
        append_status(args.status, f"stop_reason={reason} qemu_already_exited pid={pid}")
        return True
    expected = ("qemu-system-riscv64", args.expected_kernel, args.expected_overlay)
    if not all(token in cmdline for token in expected):
        append_status(
            args.status,
            f"refused_signal pid={pid} reason={reason} ownership_check_failed cmdline={cmdline}",
        )
        return False
    os.kill(pid, signal.SIGTERM)
    append_status(args.status, f"stop_reason={reason} signaled_pid={pid}")
    return True


def main() -> int:
    args = parse_args()
    args.status.parent.mkdir(parents=True, exist_ok=True)
    append_status(args.status, "monitor_started")
    started = time.monotonic()
    begin_at: float | None = None
    peak_index: int | None = None
    seen_samples: set[int] = set()
    snapshots: list[Snapshot] = []
    pid: int | None = None

    while True:
        if pid is None:
            pid = read_pid(args.pid_file)
        if pid is not None and not process_alive(pid):
            append_status(args.status, f"qemu_exited pid={pid}")
            return 0

        try:
            text = args.log.read_text(encoding="utf-8", errors="replace")
        except FileNotFoundError:
            text = ""

        now = time.monotonic()
        begin_offset = text.find("BUILDSTORM_BEGIN")
        if begin_offset >= 0 and begin_at is None:
            begin_at = now
            # Pre-timed toolchain/minibuild snapshots are useful for the
            # handshake but must never satisfy the timed peak predicate.
            seen_samples.clear()
            snapshots.clear()
            append_status(args.status, "buildstorm_begin_seen")

        if "BUILDSTORM_COMPILE ok=true" in text:
            if pid is not None:
                return 0 if terminate_owned_qemu(args, pid, "guest_compile_complete") else 2
            return 0

        timed_text = text[begin_offset:] if begin_offset >= 0 else text
        for sample, block in complete_blocks(timed_text):
            if sample in seen_samples:
                continue
            seen_samples.add(sample)
            schema = field(block, "scheduler_counter_schema_version")
            required = {
                "active": field(block, "active_tasks_excluding_collector"),
                "runnable": field(block, "runnable_total"),
                "run_ticks": field(block, "task_run_slice_ticks_total"),
                "clock_hz": field(block, "scheduler_clock_freq_hz"),
            }
            identity_ok = all(
                token in block
                for token in (
                    "task_diag pid=",
                    "current_syscall_id=",
                    "blocked_reason=",
                    "blocked_syscall_id=",
                    "runnable_wait_us=",
                )
            )
            if schema != EXPECTED_SCHEMA or any(value is None for value in required.values()) or not identity_ok:
                append_status(
                    args.status,
                    f"invalid_snapshot sample={sample} schema={schema} identity_ok={identity_ok}",
                )
                if pid is not None:
                    return 2 if not terminate_owned_qemu(args, pid, "invalid_snapshot") else 1
                return 1

            previous = snapshots[-1] if snapshots else None
            busy = None
            if previous is not None:
                delta_ticks = max(0, required["run_ticks"] - previous.run_ticks)
                busy = delta_ticks / required["clock_hz"] / args.period_seconds
            snapshot = Snapshot(
                sample=sample,
                active=required["active"],
                runnable=required["runnable"],
                run_ticks=required["run_ticks"],
                clock_hz=required["clock_hz"],
                busy_cores=busy,
            )
            snapshots.append(snapshot)
            append_status(
                args.status,
                "{}sample={} active={} runnable={} busy_cores={}".format(
                    "timed_" if begin_at is not None else "pre_",
                    sample,
                    snapshot.active,
                    snapshot.runnable,
                    "-" if busy is None else f"{busy:.3f}",
                ),
            )

            if begin_at is not None and peak_index is None and previous is not None:
                sustained_active = previous.active >= 4 and snapshot.active >= 4
                high_then_busy = previous.active >= 6 and busy is not None and busy >= 3.5
                if sustained_active or high_then_busy:
                    peak_index = len(snapshots) - 1
                    append_status(
                        args.status,
                        f"parallel_peak_triggered sample={sample} mode={'sustained' if sustained_active else 'high_busy'}",
                    )

            if peak_index is not None:
                post_peak = len(snapshots) - peak_index - 1
                if post_peak >= args.post_peak_target_samples:
                    if pid is not None:
                        return 0 if terminate_owned_qemu(args, pid, "post_peak_target_complete") else 2
                    return 0
                if post_peak >= args.post_peak_min_samples and len(snapshots) >= 4:
                    tail = snapshots[-4:]
                    tail_busy = [item.busy_cores for item in tail if item.busy_cores is not None]
                    serial_tail = (
                        all(item.active <= 2 and item.runnable <= 1 for item in tail)
                        and tail_busy
                        and sum(tail_busy) / len(tail_busy) <= 1.5
                    )
                    if serial_tail:
                        if pid is not None:
                            return 0 if terminate_owned_qemu(args, pid, "serial_tail_after_peak") else 2
                        return 0

        if begin_at is not None and peak_index is None and now - begin_at >= args.timed_timeout_seconds:
            if pid is not None:
                return 0 if terminate_owned_qemu(args, pid, "no_peak_within_timed_window") else 2
            return 0
        if begin_at is None and now - started >= args.boot_timeout_seconds:
            if pid is not None:
                return 0 if terminate_owned_qemu(args, pid, "buildstorm_begin_timeout") else 2
            append_status(args.status, "buildstorm_begin_timeout_without_pid")
            return 1
        time.sleep(2.0)


if __name__ == "__main__":
    raise SystemExit(main())
