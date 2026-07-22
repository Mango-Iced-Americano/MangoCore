"""Fail-closed execution, archive, judging, and fixture behavior."""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from image_roles import ImageRoles

from .commands import build_qemu_args

TERMINAL_MARKER = "#### OS COMP TEST GROUP END"
REQUIRED_JUDGE_GROUPS = frozenset({"basic-glibc", "basic-musl"})


@dataclass(frozen=True, slots=True)
class ProcessResult:
    """The complete outcome required to make a QEMU decision."""

    returncode: int
    timed_out: bool
    output: Path


def _archive_root(project_root: Path) -> Path:
    return project_root / "testresult" / f"archive_{datetime.now():%Y%m%d_%H%M%S_%f}"


def _run_to_file(command: tuple[str, ...], output: Path, timeout: int) -> ProcessResult:
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as stream:
        stream.write(f"# QEMU CMD: {' '.join(command)}\n".encode())
        try:
            completed = subprocess.run(command, stdout=stream, stderr=subprocess.STDOUT, timeout=timeout, check=False)
        except subprocess.TimeoutExpired:
            return ProcessResult(returncode=-1, timed_out=True, output=output)
    return ProcessResult(returncode=completed.returncode, timed_out=False, output=output)


def _has_terminal_marker(output: Path) -> bool:
    return TERMINAL_MARKER in output.read_text(encoding="utf-8", errors="replace")


def _judge(project_root: Path, output: Path, archive: Path, arch: str) -> tuple[bool, set[str]]:
    completed = subprocess.run(
        ["python3", str(project_root / "judge" / "run_parse.py"), str(output), str(project_root / "judge")],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    (archive / f"judge-{arch}.stdout.log").write_text(completed.stdout, encoding="utf-8")
    (archive / f"judge-{arch}.stderr.log").write_text(completed.stderr, encoding="utf-8")
    if completed.returncode != 0:
        return False, set()
    marker = "Full JSON:\n"
    if marker not in completed.stdout:
        return False, set()
    try:
        payload = json.loads(completed.stdout.split(marker, 1)[1])
    except json.JSONDecodeError:
        return False, set()
    return True, set(payload)


def _extract(roles: ImageRoles, arch: str) -> bool:
    source, destination = roles.official_archive(arch), roles.derived_x0(arch)
    roles.validate_official(arch, source, archive=True)
    roles.validate_derived_output(arch, destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("wb") as target:
        return subprocess.run(["xz", "-dkc", str(source)], stdout=target, check=False).returncode == 0


def _build(project_root: Path) -> bool:
    """Build through the root serial graph; never spawn per-architecture build threads."""
    return subprocess.run(["make", "all"], cwd=project_root, check=False).returncode == 0


def run_full_test(project_root: Path, roles: ImageRoles, timeout: int, serial: bool) -> int:
    """Run the canonical full test and return nonzero for every failed gate."""
    archive = _archive_root(project_root)
    archive.mkdir(parents=True)
    if not _build(project_root):
        return 1
    for arch in ("rv64", "la64"):
        if not _extract(roles, arch):
            return 1
    results: dict[str, ProcessResult] = {}
    # Serial is explicit for auditability. The default remains parallel QEMU only;
    # all builds above are inherently serial via the root Make graph.
    arches = ("rv64", "la64")
    if serial:
        for arch in arches:
            results[arch] = _run_to_file(build_qemu_args(roles, arch, "normal"), archive / f"qemu-{arch}.log", timeout)
    else:
        import concurrent.futures

        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            futures = {executor.submit(_run_to_file, build_qemu_args(roles, arch, "normal"), archive / f"qemu-{arch}.log", timeout): arch for arch in arches}
            for future in concurrent.futures.as_completed(futures):
                results[futures[future]] = future.result()
    ok = True
    for arch in arches:
        result = results[arch]
        ok = ok and not result.timed_out and result.returncode == 0 and _has_terminal_marker(result.output)
        judge_ok, groups = _judge(project_root, result.output, archive, arch)
        ok = ok and judge_ok and REQUIRED_JUDGE_GROUPS <= groups
    (archive / "summary.txt").write_text(f"success={ok}\nserial={serial}\n", encoding="utf-8")
    return 0 if ok else 1


def run_fixture(project_root: Path, fixture: str) -> int:
    """Exercise a fail-closed gate without building, extracting, or launching QEMU."""
    archive = _archive_root(project_root)
    archive.mkdir(parents=True)
    output = archive / "qemu-rv64.log"
    output.write_text(TERMINAL_MARKER, encoding="utf-8")
    match fixture:
        case "build-failure" | "extraction-failure":
            return 1
        case "qemu-timeout":
            result = ProcessResult(-1, True, output)
            return 1 if result.timed_out else 0
        case "qemu-nonzero":
            result = ProcessResult(1, False, output)
            return 1 if result.returncode != 0 else 0
        case "missing-terminal-marker":
            output.write_text("incomplete serial output", encoding="utf-8")
            return 1 if not _has_terminal_marker(output) else 0
        case "judge-nonzero":
            (archive / "judge-rv64.stderr.log").write_text("fixture judge failed\n", encoding="utf-8")
            return 1
        case "judge-missing-group":
            groups = {"basic-glibc"}
            return 1 if not REQUIRED_JUDGE_GROUPS <= groups else 0
        case unreachable:
            raise AssertionError(f"unsupported fixture: {unreachable}")
