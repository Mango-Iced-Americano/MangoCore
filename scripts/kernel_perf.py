#!/usr/bin/env python3
"""Reproducible MangoCore performance run recorder.

Raw data is written under target/perf-runs and is intentionally untracked.
The board command refuses an occupied serial device and never records process
environments. Use --sensitive for commands that may reference credentials.
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import hashlib
import json
import os
import platform
import re
import resource
import statistics
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Any, Iterable


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_RUN_ROOT = ROOT / "target" / "perf-runs"
SECRET_PATTERNS = (
    re.compile(r"(?i)(authorization\s*:\s*bearer\s+)[^\s]+"),
    re.compile(r"(?i)((?:OPENAI|ANTHROPIC|DEEPSEEK|DASHSCOPE)_API_KEY\s*[=:]\s*)[^\s]+"),
    re.compile(r"\bsk-[A-Za-z0-9_-]{8,}\b"),
)
SUSPICIOUS_COMMAND = re.compile(
    r"(?i)(authorization\s*:|bearer\s+|(?:OPENAI|ANTHROPIC|DEEPSEEK|DASHSCOPE)_API_KEY\s*=|\bsk-[A-Za-z0-9_-]{8,})"
)
STATS_FILES = {
    "core": ("boot", "taskq", "timer", "seccomp", "syscall", "ctxsw"),
    "memory_io": ("reclaim", "tlb", "heap", "pagecache", "blockio"),
    "network_runtime": ("net", "resource"),
}
CPYTHON_BENCH_PREFIX = "CPYTHON_BENCH_JSON "
SERIAL_COMMAND_LIMIT = 512


def serial_work_line(command: str, sample_id: str) -> str:
    """Return the exact shell line used to wrap one board workload."""
    work_begin = f"__PERF_WORK_BEGIN_{sample_id}__"
    work_end = f"__PERF_WORK_END_{sample_id}__"
    work_ack = f"__PERF_ACK_{sample_id}_WORK__"
    return (
        f"printf '{work_begin}\\n'; {{ {command}; }}; __perf_rc=$?; "
        f"printf '{work_end}\\n'; printf '{work_ack}\\n'"
    )


def serial_work_line_size(command: str, test: str, iteration: int = 1) -> int:
    """Calculate the deterministic wrapped line size before opening serial."""
    sample_id = f"{test}-{iteration}-00000000"
    return len(serial_work_line(command, sample_id).encode("utf-8"))


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def redact(text: str) -> str:
    result = text
    for pattern in SECRET_PATTERNS:
        if pattern.groups:
            result = pattern.sub(r"\1<redacted>", result)
        else:
            result = pattern.sub("<redacted>", result)
    return result


def run_text(command: list[str], cwd: Path = ROOT) -> str:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=30,
            check=False,
        )
        return redact(result.stdout.strip())
    except (OSError, subprocess.TimeoutExpired) as exc:
        return f"unavailable: {exc}"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def file_fingerprint(path: Path) -> dict[str, Any]:
    resolved = path.resolve()
    return {
        "path": str(resolved),
        "size": resolved.stat().st_size,
        "sha256": sha256_file(resolved),
    }


def serial_owners(device: str | None) -> list[dict[str, str]]:
    if not device:
        return []
    output = run_text(["lsof", "-Fpc", "--", device])
    if not output or output.startswith("unavailable"):
        return []
    owners: list[dict[str, str]] = []
    current: dict[str, str] = {}
    for line in output.splitlines():
        if line.startswith("p"):
            if current:
                owners.append(current)
            current = {"pid": line[1:]}
        elif line.startswith("c"):
            current["command"] = line[1:]
    if current:
        owners.append(current)
    return owners


def dirty_diff_sha256(paths: tuple[str, ...] = ()) -> str:
    command = ["git", "diff", "--binary", "HEAD"]
    if paths:
        command.extend(("--", *paths))
    result = subprocess.run(
        command,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return hashlib.sha256(result.stdout).hexdigest()


def untracked_input_fingerprint() -> tuple[str, list[dict[str, Any]]]:
    """Hash untracked source-like inputs without pulling build caches into a run."""
    result = subprocess.run(
        [
            "git", "ls-files", "--others", "--exclude-standard", "-z", "--",
            "Makefile", "os", "user", "scripts",
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    allowed_suffixes = {
        ".json", ".lock", ".md", ".py", ".rs", ".sh", ".toml", ".txt", ".yaml", ".yml",
    }
    excluded = {
        "os/src/lang_items.rs",
        "user/src/lang_items.rs",
        "os/src/hal/arch/loongarch64/linker.ld",
    }
    inputs: list[dict[str, Any]] = []
    for raw_name in result.stdout.split(b"\0"):
        if not raw_name:
            continue
        name = raw_name.decode("utf-8", errors="surrogateescape")
        relative = Path(name)
        if name in excluded or "target" in relative.parts:
            continue
        path = ROOT / relative
        if not path.is_file():
            continue
        if path.name != "Makefile" and path.suffix.lower() not in allowed_suffixes:
            continue
        inputs.append(
            {
                "path": relative.as_posix(),
                "size": path.stat().st_size,
                "sha256": sha256_file(path),
            }
        )
    inputs.sort(key=lambda item: item["path"])
    digest = hashlib.sha256()
    for item in inputs:
        digest.update(item["path"].encode("utf-8") + b"\0")
        digest.update(str(item["size"]).encode("ascii") + b"\0")
        digest.update(item["sha256"].encode("ascii") + b"\n")
    return digest.hexdigest(), inputs


def source_fingerprint() -> dict[str, Any]:
    build_inputs = (
        "Makefile",
        "os",
        "user",
        ":(exclude)os/src/lang_items.rs",
        ":(exclude)user/src/lang_items.rs",
        ":(exclude)os/src/hal/arch/loongarch64/linker.ld",
    )
    untracked_sha256, untracked_inputs = untracked_input_fingerprint()
    return {
        "head": run_text(["git", "rev-parse", "HEAD"]),
        "dirty_diff_sha256": dirty_diff_sha256(),
        "build_input_diff_sha256": dirty_diff_sha256(build_inputs),
        "untracked_input_sha256": untracked_sha256,
        "untracked_inputs": untracked_inputs,
    }


def docker_image_fingerprint(image: str) -> dict[str, str]:
    return {
        "name": image,
        "id": run_text(["docker", "image", "inspect", "--format", "{{.Id}}", image]),
        "repo_digests": run_text(
            ["docker", "image", "inspect", "--format", "{{json .RepoDigests}}", image]
        ),
    }


def append_jsonl(path: Path, value: dict[str, Any]) -> None:
    with path.open("a", encoding="utf-8") as output:
        output.write(json.dumps(value, ensure_ascii=False, sort_keys=True) + "\n")


def init_run(args: argparse.Namespace) -> int:
    run_id = args.run_id or dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir = Path(args.run_root).resolve() / run_id
    run_dir.mkdir(parents=True, exist_ok=False)
    (run_dir / "raw").mkdir()
    (run_dir / "reports").mkdir()

    artifacts = []
    for name in args.artifact:
        path = Path(name)
        if not path.is_absolute():
            path = ROOT / path
        if not path.is_file():
            raise SystemExit(f"artifact not found: {path}")
        artifacts.append(file_fingerprint(path))

    manifest = {
        "schema": 1,
        "run_id": run_id,
        "created_utc": utc_now(),
        "repository": str(ROOT),
        "git": {
            "branch": run_text(["git", "branch", "--show-current"]),
            "head": run_text(["git", "rev-parse", "HEAD"]),
            "dirty_diff_sha256": dirty_diff_sha256(),
            "status": run_text(["git", "status", "--short", "--untracked-files=no"]),
        },
        "source": source_fingerprint(),
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": sys.version.split()[0],
            "docker": run_text(["docker", "version", "--format", "{{.Client.Version}}/{{.Server.Version}}"]),
        },
        "docker_image": docker_image_fingerprint(args.docker_image),
        "serial": {"device": args.serial, "owners": serial_owners(args.serial)},
        "artifacts": artifacts,
        "notes": args.note,
        "security": {
            "environment_recorded": False,
            "redaction": "API keys, Authorization bearer values and known key variables",
        },
    }
    (run_dir / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    latest = Path(args.run_root).resolve() / "LATEST"
    latest.write_text(str(run_dir) + "\n", encoding="utf-8")
    print(run_dir)
    return 0


def add_artifacts(args: argparse.Namespace) -> int:
    run_dir = Path(args.run_dir).resolve()
    manifest_path = run_dir / "manifest.json"
    if not manifest_path.is_file():
        raise SystemExit(f"not a run directory: {run_dir}")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    known = {item["path"]: item for item in manifest.get("artifacts", [])}
    for name in args.artifact:
        path = Path(name)
        if not path.is_absolute():
            path = ROOT / path
        if not path.is_file():
            raise SystemExit(f"artifact not found: {path}")
        item = file_fingerprint(path)
        known[item["path"]] = item
    manifest["artifacts"] = sorted(known.values(), key=lambda item: item["path"])
    manifest["artifacts_updated_utc"] = utc_now()
    manifest["source_checkpoint"] = source_fingerprint()
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(manifest_path)
    return 0


def resource_delta(before: resource.struct_rusage, after: resource.struct_rusage) -> tuple[float, float]:
    return after.ru_utime - before.ru_utime, after.ru_stime - before.ru_stime


def run_command(args: argparse.Namespace) -> int:
    run_dir = Path(args.run_dir).resolve()
    if not (run_dir / "manifest.json").is_file():
        raise SystemExit(f"not a run directory: {run_dir}")
    command = args.command
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        raise SystemExit("missing command after --")

    sample_id = f"{args.test}-{args.iteration}-{uuid.uuid4().hex[:8]}"
    log_path = run_dir / "raw" / f"{sample_id}.log"
    started_utc = utc_now()
    started = time.monotonic()
    usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    with log_path.open("w", encoding="utf-8") as log:
        process = subprocess.Popen(
            command,
            cwd=args.cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        assert process.stdout is not None
        for line in process.stdout:
            clean = redact(line.rstrip("\n"))
            stamped = f"{utc_now()} {clean}\n"
            log.write(stamped)
            log.flush()
            if not args.quiet:
                print(clean, flush=True)
        returncode = process.wait()
    usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
    user_seconds, sys_seconds = resource_delta(usage_before, usage_after)
    record = {
        "schema": 1,
        "sample_id": sample_id,
        "started_utc": started_utc,
        "finished_utc": utc_now(),
        "test": args.test,
        "iteration": args.iteration,
        "arch": args.arch,
        "platform": args.platform,
        "build_mode": args.build_mode,
        "cache_state": args.cache_state,
        "command": redact(" ".join(command)),
        "cwd": str(Path(args.cwd).resolve()),
        "wall_seconds": time.monotonic() - started,
        "user_seconds": user_seconds,
        "sys_seconds": sys_seconds,
        "exit_status": returncode,
        "log": str(log_path),
        "source": source_fingerprint(),
    }
    append_jsonl(run_dir / "records.jsonl", record)
    print(json.dumps(record, ensure_ascii=False, sort_keys=True))
    return returncode


def parse_stats(lines: Iterable[str]) -> dict[str, dict[str, int]]:
    snapshots: dict[str, dict[str, int]] = {}
    current: str | None = None
    marker_re = re.compile(r"__PERF_STATS_([A-Z0-9_]+)__")
    counter_re = re.compile(r"([A-Za-z0-9_]+)=(-?\d+)\s*$")
    for line in lines:
        # The first byte printed after a shell command can share a line with the
        # prompt.  Persisted logs also prepend an ISO timestamp.  Search for the
        # framed marker instead of requiring it to occupy the complete line.
        marker_match = marker_re.search(line)
        if marker_match:
            marker = marker_match.group(1)
            if marker.endswith("_END"):
                current = None
            else:
                current = marker
                snapshots.setdefault(current, {})
            continue
        if current:
            counter_match = counter_re.search(line)
            if counter_match:
                snapshots[current][counter_match.group(1)] = int(counter_match.group(2))
    return snapshots


def parse_cpython_benchmark_events(lines: Iterable[str]) -> list[dict[str, Any]]:
    """Extract stable benchmark events from raw serial or timestamped log lines."""
    events: list[dict[str, Any]] = []
    for line in lines:
        marker = line.find(CPYTHON_BENCH_PREFIX)
        if marker < 0:
            continue
        payload = line[marker + len(CPYTHON_BENCH_PREFIX) :].strip()
        try:
            event = json.loads(payload)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict) and event.get("schema") == 1:
            events.append(event)
    return events


def counter_delta(snapshots: dict[str, dict[str, int]]) -> dict[str, int]:
    result: dict[str, int] = {}
    for name, after in snapshots.items():
        if not name.startswith("AFTER_"):
            continue
        before = snapshots.get("BEFORE_" + name.removeprefix("AFTER_"), {})
        for key, value in after.items():
            result[f"{name.removeprefix('AFTER_').lower()}.{key}"] = value - before.get(key, 0)
    return result


def board_command(args: argparse.Namespace) -> int:
    owners = serial_owners(args.serial)
    if owners:
        raise SystemExit(f"serial device is occupied; refusing to preempt: {owners}")
    if SUSPICIOUS_COMMAND.search(args.command) and not args.sensitive:
        raise SystemExit("credential-like command requires --sensitive")
    try:
        import serial  # type: ignore
    except ImportError as exc:
        raise SystemExit("pyserial is required for board capture") from exc

    run_dir = Path(args.run_dir).resolve()
    sample_id = f"{args.test}-{args.iteration}-{uuid.uuid4().hex[:8]}"
    log_path = run_dir / "raw" / f"{sample_id}.log"
    begin = f"__PERF_BEGIN_{sample_id}__"
    end_prefix = f"__PERF_END_{sample_id}__ rc="
    work_begin = f"__PERF_WORK_BEGIN_{sample_id}__"
    work_end = f"__PERF_WORK_END_{sample_id}__"
    stats_files = STATS_FILES.get(args.profile, ()) if args.profile else ()
    started_utc = utc_now()
    started = time.monotonic()
    clean_lines: list[str] = []
    exit_status: int | None = None
    work_started: float | None = None
    work_finished: float | None = None
    pending = b""
    saw_begin = False

    def send_line(port: Any, line: str) -> None:
        if len(line.encode("utf-8")) > SERIAL_COMMAND_LIMIT:
            raise RuntimeError("serial command exceeds 512 bytes; install a target script instead")
        for char in line + "\n":
            port.write(char.encode())
            if args.char_delay:
                time.sleep(args.char_delay)
        port.flush()

    def record_line(line: str, log: Any) -> None:
        nonlocal saw_begin, work_started, work_finished, exit_status
        if begin in line:
            saw_begin = True
        if saw_begin:
            clean_lines.append(line)
            log.write(f"{utc_now()} {line}\n")
            log.flush()
            if not args.quiet:
                print(line, flush=True)
        if work_begin in line:
            work_started = time.monotonic()
        if work_end in line:
            work_finished = time.monotonic()
        if end_prefix in line:
            match = re.search(re.escape(end_prefix) + r"(-?\d+)", line)
            if match:
                exit_status = int(match.group(1))

    def read_until(port: Any, log: Any, marker: str, timeout: float) -> None:
        nonlocal pending
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            pending += port.read(max(port.in_waiting, 1))
            while b"\n" in pending:
                raw, pending = pending.split(b"\n", 1)
                line = redact(raw.decode("utf-8", errors="replace").rstrip("\r"))
                record_line(line, log)
                if marker in line:
                    return
        raise TimeoutError(f"serial timeout waiting for marker: {marker}")

    def send_and_wait(port: Any, log: Any, command: str, label: str, timeout: float = 30.0) -> None:
        marker = f"__PERF_ACK_{sample_id}_{label}__"
        send_line(port, f"{command}; printf '{marker}\\n'")
        read_until(port, log, marker, timeout)

    def stats_marker(phase: str, name: str, ending: bool = False) -> str:
        suffix = "_END" if ending else ""
        return f"__PERF_STATS_{phase}_{name.upper()}{suffix}__"

    with serial.Serial(args.serial, args.baud, timeout=0.1, write_timeout=2) as port, log_path.open(
        "w", encoding="utf-8"
    ) as log:
        time.sleep(0.2)
        port.reset_input_buffer()
        # Disable terminal echo before emitting markers. Otherwise an echoed
        # command line can be mistaken for command completion, and queuing the
        # whole script can overflow the board TTY while a workload is running.
        send_line(port, "stty -echo")
        time.sleep(0.3)
        port.reset_input_buffer()
        try:
            send_line(port, f"printf '{begin}\\n'")
            read_until(port, log, begin, 10.0)
            send_and_wait(
                port,
                log,
                "test -w /sys/kernel/tracing/tracing_on && echo 0 > /sys/kernel/tracing/tracing_on",
                "TRACE_OFF",
            )
            if args.profile:
                send_and_wait(port, log, "echo 0 > /sys/kernel/stats/stats_on", "STATS_OFF")
                send_and_wait(
                    port,
                    log,
                    f"echo {args.profile} > /sys/kernel/stats/profile",
                    "PROFILE",
                )
                send_and_wait(port, log, "echo 1 > /sys/kernel/stats/reset", "RESET")
                for name in stats_files:
                    start_marker = stats_marker("BEFORE", name)
                    end_marker = stats_marker("BEFORE", name, True)
                    send_line(
                        port,
                        f"printf '{start_marker}\\n'; cat /sys/kernel/stats/{name}; "
                        f"printf '{end_marker}\\n'",
                    )
                    read_until(port, log, end_marker, 30.0)
                send_and_wait(port, log, "echo 1 > /sys/kernel/stats/stats_on", "STATS_ON")

            work_ack = f"__PERF_ACK_{sample_id}_WORK__"
            send_line(port, serial_work_line(args.command, sample_id))
            read_until(port, log, work_ack, args.timeout)

            if args.profile:
                send_and_wait(port, log, "echo 0 > /sys/kernel/stats/stats_on", "STATS_DONE")
                for name in stats_files:
                    start_marker = stats_marker("AFTER", name)
                    end_marker = stats_marker("AFTER", name, True)
                    send_line(
                        port,
                        f"printf '{start_marker}\\n'; cat /sys/kernel/stats/{name}; "
                        f"printf '{end_marker}\\n'",
                    )
                    read_until(port, log, end_marker, 30.0)
            send_line(port, f"printf '{end_prefix}%s\\n' \"$__perf_rc\"")
            read_until(port, log, end_prefix, 10.0)
        except TimeoutError:
            exit_status = 124
        finally:
            send_line(port, "stty echo")

        if exit_status is None:
            exit_status = 124

    snapshots = parse_stats(clean_lines)
    benchmark_events = parse_cpython_benchmark_events(clean_lines)
    record = {
        "schema": 1,
        "sample_id": sample_id,
        "started_utc": started_utc,
        "finished_utc": utc_now(),
        "test": args.test,
        "iteration": args.iteration,
        "arch": "la64",
        "platform": "2k1000la",
        "build_mode": args.build_mode,
        "cache_state": args.cache_state,
        "command": "<sensitive>" if args.sensitive else redact(args.command),
        "wall_seconds": (
            work_finished - work_started
            if work_started is not None and work_finished is not None
            else time.monotonic() - started
        ),
        "user_seconds": None,
        "sys_seconds": None,
        "exit_status": exit_status,
        "profile": args.profile,
        "stats": snapshots,
        "counter_delta": counter_delta(snapshots),
        "benchmark_events": benchmark_events,
        "log": str(log_path),
        "source": source_fingerprint(),
    }
    append_jsonl(run_dir / "records.jsonl", record)
    print(json.dumps(record, ensure_ascii=False, sort_keys=True))
    return exit_status


def load_records(run_dir: Path) -> list[dict[str, Any]]:
    path = run_dir / "records.jsonl"
    if not path.is_file():
        return []
    records = []
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return records


def write_csv(path: Path, rows: list[dict[str, Any]], fieldnames: list[str]) -> None:
    with path.open("w", newline="", encoding="utf-8") as output:
        writer = csv.DictWriter(output, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)


def cpython_gate(text: str) -> tuple[int, int]:
    start = "#### OS COMP TEST GROUP START cpython-isolated ####"
    end = "#### OS COMP TEST GROUP END cpython-isolated ####"
    if start in text and end in text:
        text = text.split(start, 1)[1].split(end, 1)[0]
    result = re.compile(r"\[CPYTHON\s+L\d+(?:-[A-Z0-9]+)?(?:\s+(PASS|FAIL))?\](.*)")
    passed = 0
    total = 0
    for line in text.splitlines():
        match = result.search(line)
        if not match:
            continue
        status = match.group(1)
        if status is None:
            trailing = re.search(r"\b(PASS|FAIL)\b", match.group(2))
            status = trailing.group(1) if trailing else None
        if status:
            total += 1
            passed += status == "PASS"
    return passed, total


def stamped_time(line: str) -> dt.datetime | None:
    stamp = line.split(" ", 1)[0]
    try:
        return dt.datetime.fromisoformat(stamp.removesuffix("Z") + "+00:00")
    except ValueError:
        return None


def analyze(args: argparse.Namespace) -> int:
    run_dir = Path(args.run_dir).resolve()
    records = load_records(run_dir)
    grouped: dict[tuple[str, ...], list[float]] = {}
    for record in records:
        wall = record.get("wall_seconds")
        if not isinstance(wall, (int, float)) or record.get("exit_status") != 0:
            continue
        key = tuple(
            str(record.get(field, ""))
            for field in ("test", "arch", "platform", "build_mode", "cache_state")
        )
        grouped.setdefault(key, []).append(float(wall))

    rows: list[dict[str, Any]] = []
    for key, values in sorted(grouped.items()):
        median = statistics.median(values)
        mean = statistics.fmean(values)
        cv = statistics.pstdev(values) / mean * 100 if len(values) > 1 and mean else None
        rows.append(
            {
                "test": key[0],
                "arch": key[1],
                "platform": key[2],
                "build_mode": key[3],
                "cache_state": key[4],
                "samples": len(values),
                "median_seconds": median,
                "min_seconds": min(values),
                "max_seconds": max(values),
                "cv_percent": cv,
            }
        )

    report_dir = run_dir / "reports"
    report_dir.mkdir(exist_ok=True)
    fieldnames = list(rows[0]) if rows else [
        "test", "arch", "platform", "build_mode", "cache_state", "samples",
        "median_seconds", "min_seconds", "max_seconds", "cv_percent",
    ]
    write_csv(report_dir / "summary.csv", rows, fieldnames)

    lookup = {
        (row["test"], row["arch"], row["platform"], row["cache_state"], row["build_mode"]): row
        for row in rows
    }
    probe_rows = []
    base_keys = {(key[0], key[1], key[2], key[4]) for key in grouped}
    for base in sorted(base_keys):
        if base[0].startswith(("build_", "inject_")):
            continue
        production = lookup.get((*base, "production"))
        if not production:
            continue
        for mode in ("diag_off", "diag_on"):
            candidate = lookup.get((*base, mode))
            if candidate:
                probe_rows.append(
                    {
                        "test": base[0],
                        "arch": base[1],
                        "platform": base[2],
                        "cache_state": base[3],
                        "mode": mode,
                        "delta_percent": (
                            candidate["median_seconds"] / production["median_seconds"] - 1
                        ) * 100 if production["median_seconds"] else 0.0,
                    }
                )
    write_csv(
        report_dir / "probe_tax.csv",
        probe_rows,
        ["test", "arch", "platform", "cache_state", "mode", "delta_percent"],
    )

    failures = [
        {
            "sample_id": record.get("sample_id"),
            "test": record.get("test"),
            "arch": record.get("arch"),
            "platform": record.get("platform"),
            "build_mode": record.get("build_mode"),
            "exit_status": record.get("exit_status"),
            "wall_seconds": record.get("wall_seconds"),
            "log": record.get("log"),
        }
        for record in records
        if record.get("exit_status") != 0
    ]
    write_csv(
        report_dir / "failures.csv",
        failures,
        ["sample_id", "test", "arch", "platform", "build_mode", "exit_status", "wall_seconds", "log"],
    )

    counter_rows: list[dict[str, Any]] = []
    for record in records:
        for name, value in sorted(record.get("counter_delta", {}).items()):
            counter_rows.append(
                {
                    "sample_id": record.get("sample_id"),
                    "test": record.get("test"),
                    "profile": record.get("profile"),
                    "counter": name,
                    "delta": value,
                }
            )
    write_csv(
        report_dir / "counter_deltas.csv",
        counter_rows,
        ["sample_id", "test", "profile", "counter", "delta"],
    )

    benchmark_sample_rows: list[dict[str, Any]] = []
    benchmark_summary_rows: list[dict[str, Any]] = []
    benchmark_phase_rows: list[dict[str, Any]] = []
    for record in records:
        events = record.get("benchmark_events")
        if not isinstance(events, list):
            events = []
        if not events:
            log_name = record.get("log")
            if log_name and Path(log_name).is_file():
                events = parse_cpython_benchmark_events(
                    Path(log_name).read_text(encoding="utf-8", errors="replace").splitlines()
                )

        environment: dict[str, Any] = {}
        for event in events:
            if not isinstance(event, dict):
                continue
            event_type = event.get("type")
            if event_type == "environment":
                environment = event
                continue
            common = {
                "sample_id": record.get("sample_id"),
                "test": record.get("test"),
                "benchmark": event.get("benchmark"),
                "arch": record.get("arch"),
                "platform": record.get("platform"),
                "build_mode": record.get("build_mode"),
                "cache_state": record.get("cache_state"),
                "source_revision": environment.get("source_revision"),
                "pyperformance_revision": environment.get("pyperformance_revision"),
                "suite_sha256": environment.get("suite_sha256"),
                "python_version": environment.get("python_version"),
                "machine": environment.get("machine"),
            }
            if event_type == "sample":
                benchmark_sample_rows.append(
                    {
                        **common,
                        "sample": event.get("sample"),
                        "elapsed_seconds": event.get("elapsed_seconds"),
                        "elapsed_ns": event.get("elapsed_ns"),
                        "user_seconds": event.get("user_seconds"),
                        "sys_seconds": event.get("sys_seconds"),
                        "max_rss": event.get("max_rss"),
                        "result": event.get("result"),
                    }
                )
                phase_metrics = event.get("phase_metrics")
                if isinstance(phase_metrics, dict):
                    for phase, value in sorted(phase_metrics.items()):
                        benchmark_phase_rows.append(
                            {
                                **common,
                                "sample": event.get("sample"),
                                "phase": phase,
                                "value": value,
                                "unit": "ns" if str(phase).endswith("_ns") else "count",
                            }
                        )
            elif event_type in ("summary", "error"):
                benchmark_summary_rows.append(
                    {
                        **common,
                        "status": event.get("status"),
                        "samples": event.get("samples"),
                        "warmups": event.get("warmups", environment.get("warmups")),
                        "median_seconds": (
                            event.get("median_ns") / 1_000_000_000.0
                            if isinstance(event.get("median_ns"), (int, float))
                            else None
                        ),
                        "min_seconds": (
                            event.get("min_ns") / 1_000_000_000.0
                            if isinstance(event.get("min_ns"), (int, float))
                            else None
                        ),
                        "max_seconds": (
                            event.get("max_ns") / 1_000_000_000.0
                            if isinstance(event.get("max_ns"), (int, float))
                            else None
                        ),
                        "mean_seconds": (
                            event.get("mean_ns") / 1_000_000_000.0
                            if isinstance(event.get("mean_ns"), (int, float))
                            else None
                        ),
                        "cv_percent": event.get("cv_percent"),
                        "error_type": event.get("error_type"),
                        "error": event.get("error"),
                        "perf_counter_resolution_seconds": environment.get(
                            "perf_counter_resolution_seconds"
                        ),
                        "tmpdir": environment.get("tmpdir"),
                        "pycache_prefix": environment.get("pycache_prefix"),
                    }
                )

    benchmark_common_fields = [
        "sample_id", "test", "benchmark", "arch", "platform", "build_mode",
        "cache_state", "source_revision", "pyperformance_revision", "suite_sha256",
        "python_version", "machine",
    ]
    write_csv(
        report_dir / "cpython_bench_samples.csv",
        benchmark_sample_rows,
        benchmark_common_fields
        + ["sample", "elapsed_seconds", "elapsed_ns", "user_seconds", "sys_seconds", "max_rss", "result"],
    )
    write_csv(
        report_dir / "cpython_bench_summary.csv",
        benchmark_summary_rows,
        benchmark_common_fields
        + [
            "status", "samples", "warmups", "median_seconds", "min_seconds", "max_seconds",
            "mean_seconds", "cv_percent", "error_type", "error",
            "perf_counter_resolution_seconds", "tmpdir", "pycache_prefix",
        ],
    )
    write_csv(
        report_dir / "cpython_bench_phases.csv",
        benchmark_phase_rows,
        benchmark_common_fields + ["sample", "phase", "value", "unit"],
    )

    boot_rows: list[dict[str, Any]] = []
    gate_rows: list[dict[str, Any]] = []
    board_boot_rows: list[dict[str, Any]] = []
    for record in records:
        log_name = record.get("log")
        if not log_name or not Path(log_name).is_file():
            continue
        text = Path(log_name).read_text(encoding="utf-8", errors="replace")
        boot_values: dict[str, int] = {}
        for name, value in re.findall(r"\b(clock_freq_hz|boot_[a-z]+_ticks)=(\d+)", text):
            boot_values[name] = int(value)
        if boot_values:
            boot_rows.append(
                {
                    "sample_id": record.get("sample_id"),
                    "arch": record.get("arch"),
                    "platform": record.get("platform"),
                    **boot_values,
                }
            )
        if record.get("test") == "cpython_l3_l9":
            passed, total = cpython_gate(text)
            gate_rows.append(
                {
                    "sample_id": record.get("sample_id"),
                    "arch": record.get("arch"),
                    "platform": record.get("platform"),
                    "passed": passed,
                    "total": total,
                    "exit_status": record.get("exit_status"),
                    "wall_seconds": record.get("wall_seconds"),
                }
            )
        if record.get("test") == "board_cold_boot" and record.get("exit_status") == 0:
            booting = None
            ready = None
            for line in text.splitlines():
                if "[console] booting" in line:
                    booting = stamped_time(line)
                if "[apk-persist-shell] ready:" in line:
                    ready = stamped_time(line)
            if booting and ready:
                board_boot_rows.append(
                    {
                        "sample_id": record.get("sample_id"),
                        "build_mode": record.get("build_mode"),
                        "boot_to_shell_seconds": (ready - booting).total_seconds(),
                    }
                )

    boot_fields = [
        "sample_id", "arch", "platform", "clock_freq_hz", "boot_console_ticks",
        "boot_mm_ticks", "boot_drivers_ticks", "boot_net_ticks", "boot_fs_ticks",
        "boot_initproc_ticks", "boot_scheduler_ticks",
    ]
    write_csv(report_dir / "boot_milestones.csv", boot_rows, boot_fields)
    write_csv(
        report_dir / "cpython_gates.csv",
        gate_rows,
        ["sample_id", "arch", "platform", "passed", "total", "exit_status", "wall_seconds"],
    )
    write_csv(
        report_dir / "board_boot.csv",
        board_boot_rows,
        ["sample_id", "build_mode", "boot_to_shell_seconds"],
    )

    report = [
        "# MangoCore performance run summary",
        "",
        f"Generated: {utc_now()}",
        "",
        "| test | arch | platform | build | cache | n | median s | min s | max s | CV % |",
        "|---|---|---|---|---|---:|---:|---:|---:|---:|",
    ]
    for row in rows:
        cv_text = f"{row['cv_percent']:.2f}" if row["cv_percent"] is not None else "n/a"
        report.append(
            f"| {row['test']} | {row['arch']} | {row['platform']} | {row['build_mode']} | "
            f"{row['cache_state']} | {row['samples']} | {row['median_seconds']:.6f} | "
            f"{row['min_seconds']:.6f} | {row['max_seconds']:.6f} | {cv_text} |"
        )
    (report_dir / "summary.md").write_text("\n".join(report) + "\n", encoding="utf-8")
    print(report_dir)
    return 0


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    sub = result.add_subparsers(dest="subcommand", required=True)

    init = sub.add_parser("init", help="create a fingerprinted run directory")
    init.add_argument("--run-root", default=str(DEFAULT_RUN_ROOT))
    init.add_argument("--run-id")
    init.add_argument("--docker-image", default="zhouzhouyi/os-contest:20260104")
    init.add_argument("--artifact", action="append", default=[])
    init.add_argument("--serial", default="/dev/cu.wchusbserial120")
    init.add_argument("--note", action="append", default=[])
    init.set_defaults(func=init_run)

    artifact = sub.add_parser("artifact", help="fingerprint built images into a run manifest")
    artifact.add_argument("--run-dir", required=True)
    artifact.add_argument("--artifact", action="append", required=True)
    artifact.set_defaults(func=add_artifacts)

    run = sub.add_parser("run", help="record one host/QEMU/build command")
    run.add_argument("--run-dir", required=True)
    run.add_argument("--test", required=True)
    run.add_argument("--iteration", type=int, default=1)
    run.add_argument("--arch", default="host")
    run.add_argument("--platform", default="host")
    run.add_argument("--build-mode", default="production")
    run.add_argument("--cache-state", default="unspecified")
    run.add_argument("--cwd", default=str(ROOT))
    run.add_argument("--quiet", action="store_true")
    run.add_argument("command", nargs=argparse.REMAINDER)
    run.set_defaults(func=run_command)

    board = sub.add_parser("board", help="record one command on a free serial console")
    board.add_argument("--run-dir", required=True)
    board.add_argument("--serial", default="/dev/cu.wchusbserial120")
    board.add_argument("--baud", type=int, default=115200)
    board.add_argument("--test", required=True)
    board.add_argument("--iteration", type=int, default=1)
    board.add_argument("--build-mode", choices=("production", "diag_off", "diag_on"), required=True)
    board.add_argument("--cache-state", default="unspecified")
    board.add_argument("--profile", choices=tuple(STATS_FILES))
    board.add_argument("--timeout", type=float, default=300.0)
    board.add_argument("--char-delay", type=float, default=0.005)
    board.add_argument("--sensitive", action="store_true")
    board.add_argument("--quiet", action="store_true")
    board.add_argument("--command", required=True)
    board.set_defaults(func=board_command)

    summary = sub.add_parser("analyze", help="generate CSV and Markdown summaries")
    summary.add_argument("--run-dir", required=True)
    summary.set_defaults(func=analyze)
    return result


def main() -> int:
    args = parser().parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
