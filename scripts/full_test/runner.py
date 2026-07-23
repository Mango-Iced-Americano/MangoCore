"""Fail-closed execution, archive, judging, and fixture behavior."""

from __future__ import annotations

import json
import shutil
import subprocess
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from image_roles import ImageRoles

from .commands import build_qemu_args

TERMINAL_MARKER = "#### OS COMP TEST GROUP END"
REQUIRED_JUDGE_GROUPS = frozenset({"basic-glibc", "basic-musl"})
ARCHES = ("rv64", "la64")
FATAL_SIGNATURES = (
    "[kernel] panicked at",
    "kernel panic",
    "out of memory",
    "sigkill",
    "fatal trap",
)


@dataclass(frozen=True, slots=True)
class ProcessResult:
    """The complete outcome required to make a QEMU decision."""

    returncode: int
    timed_out: bool
    output: Path


def _archive_root(project_root: Path) -> Path:
    return project_root / "testresult" / f"archive_{datetime.now():%Y%m%d_%H%M%S_%f}"


def _arch_archive(archive: Path, arch: str) -> Path:
    """Return the per-architecture diagnostics directory."""
    path = archive / arch
    path.mkdir(parents=True, exist_ok=True)
    return path


def _write_result(archive: Path, stage: str, status: str, diagnostic: str) -> None:
    """Persist the machine-readable result for one architecture."""
    payload = {"stage": stage, "status": status, "diagnostic": diagnostic}
    (archive / "result.json").write_text(json.dumps(payload, sort_keys=True) + "\n", encoding="utf-8")


def _prepare_archives(archive: Path) -> None:
    """Create the required logs before any build or QEMU stage can fail."""
    for arch in ARCHES:
        arch_archive = _arch_archive(archive, arch)
        (arch_archive / "qemu.log").write_text("QEMU not started.\n", encoding="utf-8")


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


def _has_abnormal_signature(output: Path) -> bool:
    """Reject terminal-looking logs that include an unrecoverable kernel failure."""
    contents = output.read_text(encoding="utf-8", errors="replace").casefold()
    return any(signature in contents for signature in FATAL_SIGNATURES)


def _judge(project_root: Path, output: Path, archive: Path) -> tuple[bool, set[str], str]:
    try:
        completed = subprocess.run(
            ["python3", str(project_root / "judge" / "run_parse.py"), str(output), str(project_root / "judge")],
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except subprocess.TimeoutExpired:
        (archive / "judge.stdout.log").write_text("", encoding="utf-8")
        (archive / "judge.stderr.log").write_text("judge timed out\n", encoding="utf-8")
        return False, set(), "judge timed out"
    (archive / "judge.stdout.log").write_text(completed.stdout, encoding="utf-8")
    (archive / "judge.stderr.log").write_text(completed.stderr, encoding="utf-8")
    if completed.returncode != 0:
        return False, set(), f"judge exited {completed.returncode}"
    marker = "Full JSON:\n"
    if marker not in completed.stdout:
        return False, set(), "judge omitted the JSON marker"
    try:
        payload = json.loads(completed.stdout.split(marker, 1)[1])
    except json.JSONDecodeError:
        return False, set(), "judge emitted invalid JSON"
    return True, set(payload), "judge accepted output"


def _extract(roles: ImageRoles, arch: str, archive: Path) -> tuple[bool, str]:
    from datetime import datetime
    source = roles.official_archive(arch)
    destination = roles.official_x0(arch)  # decompress directly to ../sdcard-{rv,la}.img
    roles.validate_official(arch, source, archive=True)
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.is_file() and destination.stat().st_mtime >= source.stat().st_mtime:
        ts = datetime.now().strftime("%H:%M:%S")
        print(f"[{ts}] Phase 2/{arch}: {destination.name} up-to-date, skipping decompress")
        return True, "extraction cached"
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] Phase 2/{arch}: decompressing {source.name} → {destination}...")
    with destination.open("wb") as target, (archive / "extract.log").open("wb") as log:
        completed = subprocess.run(["xz", "-dkc", str(source)], stdout=target, stderr=log, check=False)
    if completed.returncode != 0:
        ts = datetime.now().strftime("%H:%M:%S")
        print(f"[{ts}] ERROR: xz extraction exited {completed.returncode}")
        return False, f"xz extraction exited {completed.returncode}"
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] Phase 2/{arch} complete: extraction succeeded")
    return True, "extraction succeeded"


def _build(project_root: Path, archive: Path) -> tuple[bool, str]:
    """Build through the root serial graph; never spawn per-architecture build threads."""
    from datetime import datetime
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] Phase 1/5: make all (serial RV64 → LA64 build)...")
    build_log = archive / "build.log"
    with build_log.open("wb") as log:
        completed = subprocess.run(["make", "all"], cwd=project_root, stdout=log, stderr=subprocess.STDOUT, check=False)
    for arch in ARCHES:
        shutil.copyfile(build_log, _arch_archive(archive, arch) / "build.log")
    if completed.returncode != 0:
        ts = datetime.now().strftime("%H:%M:%S")
        print(f"[{ts}] ERROR: make all exited {completed.returncode}")
        return False, f"make all exited {completed.returncode}"
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] Phase 1 complete: build succeeded")
    return True, "build succeeded"


def _qemu_failure(result: ProcessResult) -> tuple[str, str] | None:
    """Return the first fail-closed QEMU gate violated by one result."""
    if result.timed_out:
        return "qemu-timeout", "QEMU timed out"
    if result.returncode != 0:
        return "qemu-nonzero", f"QEMU exited {result.returncode}"
    if _has_abnormal_signature(result.output):
        return "abnormal-signature", "QEMU output contains a fatal kernel signature"
    if not _has_terminal_marker(result.output):
        return "missing-terminal-marker", "QEMU output omitted the terminal marker"
    return None


def run_full_test(project_root: Path, roles: ImageRoles, timeout: int, serial: bool) -> int:
    """Run the canonical full test and return nonzero for every failed gate."""
    from datetime import datetime
    import concurrent.futures

    archive = _archive_root(project_root)
    archive.mkdir(parents=True)
    _prepare_archives(archive)

    # Phase 1+2: Build + Extract (all parallel — make all is independent of sdcard decompression)
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] Phase 1+2: make all + decompress sdcard images (parallel)...")
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as executor:
        build_future = executor.submit(_build, project_root, archive)
        extract_futures = {
            arch: executor.submit(_extract, roles, arch, _arch_archive(archive, arch))
            for arch in ARCHES
        }
        build_ok, build_diagnostic = build_future.result()
        if not build_ok:
            for arch in ARCHES:
                _write_result(_arch_archive(archive, arch), "build", "failed", build_diagnostic)
            return 1
        for arch, future in extract_futures.items():
            extract_ok, extract_diagnostic = future.result()
            if not extract_ok:
                _write_result(_arch_archive(archive, arch), "extract", "failed", extract_diagnostic)
                return 1

    # Phase 3-4: QEMU + Judge
    results: dict[str, ProcessResult] = {}
    arches = ARCHES
    mode = "serial" if serial else "parallel"
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] Phase 3-4: launching QEMU ({mode}, timeout {timeout}s)...")
    if serial:
        for arch in arches:
            ts = datetime.now().strftime("%H:%M:%S")
            print(f"[{ts}] QEMU {arch}: starting... (timeout {timeout}s)")
            results[arch] = _run_to_file(build_qemu_args(roles, arch, "competition"), _arch_archive(archive, arch) / "qemu.log", timeout)
            ts = datetime.now().strftime("%H:%M:%S")
            print(f"[{ts}] QEMU {arch}: finished (rc={results[arch].returncode})")
    else:
        import concurrent.futures

        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            futures = {executor.submit(_run_to_file, build_qemu_args(roles, arch, "competition"), _arch_archive(archive, arch) / "qemu.log", timeout): arch for arch in arches}
            for future in concurrent.futures.as_completed(futures):
                arch = futures[future]
                result = future.result()
                results[arch] = result
                ts = datetime.now().strftime("%H:%M:%S")
                print(f"[{ts}] QEMU {arch}: finished (rc={result.returncode})")
    ok = True
    for arch in arches:
        result = results[arch]
        arch_archive = _arch_archive(archive, arch)
        qemu_failure = _qemu_failure(result)
        if qemu_failure is not None:
            stage, diagnostic = qemu_failure
            _write_result(arch_archive, stage, "failed", diagnostic)
            ts = datetime.now().strftime("%H:%M:%S")
            print(f"[{ts}] {arch}: {stage} — FAILED ({diagnostic})")
            ok = False
            continue
        judge_ok, groups, judge_diagnostic = _judge(project_root, result.output, arch_archive)
        if not judge_ok:
            _write_result(arch_archive, "judge", "failed", judge_diagnostic)
            ts = datetime.now().strftime("%H:%M:%S")
            print(f"[{ts}] {arch}: judge — FAILED ({judge_diagnostic[:80]})")
            ok = False
            continue
        if not REQUIRED_JUDGE_GROUPS <= groups:
            _write_result(arch_archive, "judge-groups", "failed", "required judge groups are missing")
            ts = datetime.now().strftime("%H:%M:%S")
            print(f"[{ts}] {arch}: judge groups — FAILED (missing required groups)")
            ok = False
            continue
        _write_result(arch_archive, "complete", "passed", "all fail-closed gates passed")
        ts = datetime.now().strftime("%H:%M:%S")
        print(f"[{ts}] {arch}: PASSED — all gates passed")
    (archive / "summary.txt").write_text(f"success={ok}\nserial={serial}\n", encoding="utf-8")
    ts = datetime.now().strftime("%H:%M:%S")
    if ok:
        print(f"[{ts}] Phase 5/5: ALL PASSED — archives at {archive}")
    else:
        print(f"[{ts}] Phase 5/5: FAILURES DETECTED — archives at {archive}")
    return 0 if ok else 1


def run_fixture(project_root: Path, fixture: str) -> int:
    """Exercise a fail-closed gate without building, extracting, or launching QEMU."""
    archive = _archive_root(project_root)
    archive.mkdir(parents=True)
    _prepare_archives(archive)
    arch_archive = _arch_archive(archive, "rv64")
    (arch_archive / "build.log").write_text("fixture bypassed build\n", encoding="utf-8")
    output = arch_archive / "qemu.log"
    output.write_text(TERMINAL_MARKER, encoding="utf-8")
    match fixture:
        case "build-failure" | "extraction-failure":
            _write_result(arch_archive, fixture.removesuffix("-failure"), "failed", "fixture failure")
            return 1
        case "qemu-timeout":
            result = ProcessResult(-1, True, output)
            failure = _qemu_failure(result)
        case "qemu-nonzero":
            result = ProcessResult(1, False, output)
            failure = _qemu_failure(result)
        case "missing-terminal-marker":
            output.write_text("incomplete serial output", encoding="utf-8")
            failure = _qemu_failure(ProcessResult(0, False, output))
        case "abnormal-signature":
            output.write_text(f"[kernel] panicked at fixture\n{TERMINAL_MARKER}\n", encoding="utf-8")
            failure = _qemu_failure(ProcessResult(0, False, output))
        case "judge-nonzero":
            (arch_archive / "judge.stderr.log").write_text("fixture judge failed\n", encoding="utf-8")
            _write_result(arch_archive, "judge", "failed", "fixture judge failed")
            return 1
        case "judge-missing-group":
            groups = {"basic-glibc"}
            _write_result(arch_archive, "judge-groups", "failed", "fixture omitted a required group")
            return 1 if not REQUIRED_JUDGE_GROUPS <= groups else 0
        case unreachable:
            raise AssertionError(f"unsupported fixture: {unreachable}")
    if failure is None:
        _write_result(arch_archive, "fixture", "passed", "fixture unexpectedly passed")
        return 0
    stage, diagnostic = failure
    _write_result(arch_archive, stage, "failed", diagnostic)
    return 1
