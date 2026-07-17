#!/usr/bin/env python3
"""Atomically side-load a CPython runtime into the board's P4 ext4 partition."""

from __future__ import annotations

import argparse
import functools
import hashlib
import http.server
import shlex
import subprocess
import sys
import tarfile
import threading
from pathlib import Path, PurePosixPath

from kernel_perf import SERIAL_COMMAND_LIMIT, serial_work_line_size


ROOT = Path(__file__).resolve().parents[1]
HARNESS = ROOT / "scripts" / "kernel_perf.py"
BOOTSTRAP_PYTHON = "/tools/tests/cpython/python3-wrapper.sh"


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: object) -> None:
        return


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_archive(path: Path) -> None:
    def stays_below_root(candidate: PurePosixPath) -> bool:
        depth = 0
        for part in candidate.parts:
            if part in ("", "."):
                continue
            if part == "..":
                if depth == 0:
                    return False
                depth -= 1
            else:
                depth += 1
        return True

    with tarfile.open(path, "r:xz") as archive:
        for member in archive.getmembers():
            name = PurePosixPath(member.name)
            if name.is_absolute() or not stays_below_root(name):
                raise SystemExit("unsafe archive member: %s" % member.name)
            if not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
                raise SystemExit("unsupported special archive member: %s" % member.name)
            if member.issym() or member.islnk():
                target = PurePosixPath(member.linkname)
                if target.is_absolute():
                    raise SystemExit("absolute archive link: %s" % member.name)
                resolved = name.parent / target if member.issym() else target
                if not stays_below_root(resolved):
                    raise SystemExit("escaping archive link: %s" % member.name)


def board_phase(args: argparse.Namespace, name: str, command: str, timeout: int) -> int:
    argv = [
        sys.executable,
        str(HARNESS),
        "board",
        "--run-dir",
        str(args.run_dir),
        "--serial",
        args.serial,
        "--baud",
        str(args.baud),
        "--test",
        name,
        "--build-mode",
        args.build_mode,
        "--cache-state",
        "deployment",
        "--timeout",
        str(timeout),
        "--command",
        command,
    ]
    if args.quiet:
        argv.append("--quiet")
    return subprocess.run(argv, cwd=ROOT, check=False).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--serial", default="/dev/cu.wchusbserial120")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--port", type=int, default=8009)
    parser.add_argument("--target-parent", default="/persist/pyperf/r")
    parser.add_argument(
        "--build-mode",
        choices=("production", "diag_off", "diag_on"),
        default="diag_on",
    )
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    artifact = args.artifact.resolve()
    if not artifact.is_file():
        parser.error("artifact does not exist: %s" % artifact)
    if not args.run_dir.is_dir() and not args.dry_run:
        parser.error("run directory does not exist: %s" % args.run_dir)
    raw_parent = args.target_parent.rstrip("/")
    parent_path = PurePosixPath(raw_parent)
    if (
        not parent_path.is_absolute()
        or len(parent_path.parts) < 3
        or parent_path.parts[1] != "persist"
        or ".." in parent_path.parts
        or any(ord(char) < 32 for char in raw_parent)
    ):
        parser.error("target parent must be below /persist on P4 ext4")
    parent = str(parent_path)

    validate_archive(artifact)
    digest = sha256(artifact)
    short = digest[:12]
    final = "%s/s-%s" % (parent, short)
    staging = "%s/.s-%s.staging" % (parent, short)
    remote_archive = "%s/.%s.part" % (parent, artifact.name)
    url = "http://%s:%d/%s" % (args.host_ip, args.port, artifact.name)

    download = "import urllib.request as u;u.urlretrieve(%r,%r)" % (url, remote_archive)
    # The archive is validated member-by-member on the host and then verified
    # byte-for-byte on the board before extraction.  Use BusyBox's native xz
    # path here: bootstrapping through the old, non-strict CPython runtime can
    # spend minutes in user-unaligned trap emulation while unpacking this very
    # runtime, and may exceed the serial harness timeout.
    extract = "/bin/busybox tar -xJf %s -C %s" % tuple(
        map(shlex.quote, (remote_archive, staging))
    )
    verify = "printf '%%s\\n' %s|/bin/busybox sha256sum -c -" % shlex.quote(
        "%s  %s" % (digest, remote_archive)
    )
    p4_mount_check = (
        "/bin/busybox grep -Eq "
        "'^/persist[[:space:]]+/persist[[:space:]]+ext4[[:space:]]+rw([,[:space:]])' "
        "/proc/mounts&&test ! -L /persist&&test -w /persist&&"
        "test \"$(blockdev --getsize64 /dev/sda4)\" = 4294967296"
    )
    parent_check = "mkdir -p %s&&test \"$(/bin/busybox readlink -f %s)\" = %s" % tuple(
        map(shlex.quote, (parent, parent, parent))
    )
    phases = (
        ("strict_p4_mount", p4_mount_check, 120),
        ("strict_p4_parent", parent_check, 120),
        (
            "strict_runtime_prepare",
            "test ! -e %s&&rm -rf %s&&rm -f %s&&mkdir %s"
            % tuple(map(shlex.quote, (final, staging, remote_archive, staging))),
            120,
        ),
        (
            "strict_runtime_download",
            "%s -c %s" % (BOOTSTRAP_PYTHON, shlex.quote(download)),
            600,
        ),
        (
            "strict_runtime_sha256",
            verify,
            300,
        ),
        (
            "strict_runtime_extract",
            extract,
            900,
        ),
        (
            "strict_runtime_smoke",
            shlex.quote(staging + "/strict_runtime_smoke.sh"),
            300,
        ),
        (
            "strict_runtime_publish",
            "test ! -e %s&&sync&&mv %s %s&&sync"
            % tuple(map(shlex.quote, (final, staging, final))),
            300,
        ),
        (
            "strict_runtime_cleanup",
            "rm -f %s" % shlex.quote(remote_archive),
            120,
        ),
    )

    print("artifact_sha256=%s" % digest)
    print("target=%s" % final)
    for name, command, _ in phases:
        wrapped_bytes = serial_work_line_size(command, name)
        if wrapped_bytes > SERIAL_COMMAND_LIMIT:
            raise SystemExit(
                "wrapped board command exceeds %d bytes: %s (%d)"
                % (SERIAL_COMMAND_LIMIT, name, wrapped_bytes)
            )
        if args.dry_run:
            print(
                "%s command_bytes=%d wrapped_bytes=%d"
                % (name, len(command.encode("utf-8")), wrapped_bytes)
            )
            print(command)
    if args.dry_run:
        return 0

    handler = functools.partial(QuietHandler, directory=str(artifact.parent))
    server = http.server.ThreadingHTTPServer((args.host_ip, args.port), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        for name, command, timeout in phases:
            if board_phase(args, name, command, timeout):
                return 1
        return 0
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


if __name__ == "__main__":
    raise SystemExit(main())
