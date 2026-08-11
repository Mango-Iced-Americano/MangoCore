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
    busy_cores_30s: float | None
    build_tasks: int


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--log", required=True, type=Path)
    parser.add_argument("--pid-file", required=True, type=Path)
    parser.add_argument("--status", required=True, type=Path)
    parser.add_argument("--expected-kernel", required=True)
    parser.add_argument("--expected-overlay", required=True)
    parser.add_argument("--period-seconds", type=float, default=5.0)
    parser.add_argument("--timed-timeout-seconds", type=float, default=900.0)
    parser.add_argument("--hard-timed-timeout-seconds", type=float, default=0.0)
    parser.add_argument("--boot-timeout-seconds", type=float, default=1800.0)
    parser.add_argument("--post-peak-min-samples", type=int, default=36)
    parser.add_argument("--post-peak-target-samples", type=int, default=60)
    return parser.parse_args()


def append_status(path: Path, message: str) -> None:
    stamp = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    with path.open("a", encoding="utf-8") as status:
        status.write(f"{stamp} {message}\n")
        status.flush()


def buildstorm_compile_succeeded(text: str) -> bool:
    """Match the ordered result marker without requiring adjacent fields."""

    return re.search(r"(?m)^BUILDSTORM_COMPILE\b.*\bok=true\b", text) is not None


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


def child_pids(pid: int) -> list[int]:
    try:
        text = Path(f"/proc/{pid}/task/{pid}/children").read_text(encoding="ascii")
    except (FileNotFoundError, OSError):
        return []
    return [int(value) for value in text.split() if value.isdigit()]


def owned_qemu_pid(args: argparse.Namespace, pid: int) -> int | None:
    """Resolve a launcher PID to its QEMU child without touching other runs."""
    pending = [pid]
    seen: set[int] = set()
    expected = ("qemu-system-riscv64", args.expected_kernel, args.expected_overlay)
    while pending:
        candidate = pending.pop(0)
        if candidate in seen:
            continue
        seen.add(candidate)
        try:
            cmdline = Path(f"/proc/{candidate}/cmdline").read_bytes().replace(
                b"\0", b" "
            ).decode("utf-8", errors="replace")
            executable = Path(os.readlink(f"/proc/{candidate}/exe")).name
        except FileNotFoundError:
            continue
        # A launcher shell contains the complete QEMU command in its own
        # cmdline, so token matching alone can select and terminate the shell
        # while leaving QEMU orphaned. Require the resolved executable too.
        if executable == expected[0] and all(token in cmdline for token in expected):
            return candidate
        pending.extend(child_pids(candidate))
    return None


def terminate_owned_qemu(args: argparse.Namespace, pid: int, reason: str) -> bool:
    qemu_pid = owned_qemu_pid(args, pid)
    if qemu_pid is None:
        if not process_alive(pid):
            append_status(args.status, f"stop_reason={reason} qemu_already_exited pid={pid}")
            return True
        try:
            cmdline = Path(f"/proc/{pid}/cmdline").read_bytes().replace(
                b"\0", b" "
            ).decode("utf-8", errors="replace")
        except FileNotFoundError:
            append_status(args.status, f"stop_reason={reason} qemu_already_exited pid={pid}")
            return True
        append_status(
            args.status,
            f"refused_signal pid={pid} reason={reason} ownership_check_failed cmdline={cmdline}",
        )
        return False
    os.kill(qemu_pid, signal.SIGTERM)
    append_status(args.status, f"stop_reason={reason} signaled_pid={qemu_pid} launcher_pid={pid}")
    return True


def build_task_count(block: str) -> int:
    count = 0
    for line in block.splitlines():
        if not line.startswith("task_diag ") or "pid=3 " in line:
            continue
        crate = re.search(r"\bcrate=([^ ]+)", line)
        current_cpu = re.search(r"\bcurrent_cpu=([^ ]+)", line)
        state = re.search(r"\bstate=([^ ]+)", line)
        if crate and crate.group(1) not in {"-", "none", "None"} and (
            (current_cpu and current_cpu.group(1) != "-")
            or (state and state.group(1) in {"Ready", "Running", "Runnable"})
        ):
            count += 1
    return count


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

        if buildstorm_compile_succeeded(text):
            if pid is not None:
                return 0 if terminate_owned_qemu(args, pid, "guest_compile_complete") else 2
            return 0

        if (
            begin_at is not None
            and args.hard_timed_timeout_seconds > 0
            and now - begin_at >= args.hard_timed_timeout_seconds
        ):
            if pid is not None:
                return (
                    0
                    if terminate_owned_qemu(args, pid, "hard_timed_window_complete")
                    else 2
                )
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
            busy_30s = None
            if len(snapshots) >= 6:
                window = snapshots[-6]
                delta_ticks = max(0, required["run_ticks"] - window.run_ticks)
                elapsed_seconds = max(
                    args.period_seconds * (sample - window.sample),
                    args.period_seconds,
                )
                busy_30s = delta_ticks / required["clock_hz"] / elapsed_seconds
            snapshot = Snapshot(
                sample=sample,
                active=required["active"],
                runnable=required["runnable"],
                run_ticks=required["run_ticks"],
                clock_hz=required["clock_hz"],
                busy_cores=busy,
                busy_cores_30s=busy_30s,
                build_tasks=build_task_count(block),
            )
            snapshots.append(snapshot)
            append_status(
                args.status,
                "{}sample={} active={} runnable={} build_tasks={} busy_cores={} busy_cores_30s={}".format(
                    "timed_" if begin_at is not None else "pre_",
                    sample,
                    snapshot.active,
                    snapshot.runnable,
                    snapshot.build_tasks,
                    "-" if busy is None else f"{busy:.3f}",
                    "-" if busy_30s is None else f"{busy_30s:.3f}",
                ),
            )

            if begin_at is not None and peak_index is None and previous is not None:
                sustained_active = previous.active >= 4 and snapshot.active >= 4
                high_then_busy = (
                    previous.active >= 6
                    and snapshot.busy_cores_30s is not None
                    and snapshot.busy_cores_30s >= 3.5
                )
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
