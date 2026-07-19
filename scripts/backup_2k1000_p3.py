#!/usr/bin/env python3
"""Deploy and run the bounded P3-to-P4 backup on a MangoCore board shell."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
BOARD_SCRIPT = ROOT / "scripts" / "board" / "backup_2k1000_p3.sh"
HARNESS = ROOT / "scripts" / "kernel_perf.py"
EXPECTED_P3_START = 0xA80800
BACKUP_ID = re.compile(r"^[A-Za-z0-9._-]+$")


def http_server_command(directory: Path, host: str, port: int) -> list[str]:
    ruby = Path("/usr/bin/ruby")
    if sys.platform == "darwin" and ruby.is_file():
        return [
            str(ruby), "-run", "-e", "httpd", str(directory),
            "-p", str(port), "-b", host,
        ]
    return [
        sys.executable, "-m", "http.server", str(port),
        "--bind", host, "--directory", str(directory),
    ]


def run_board(args: argparse.Namespace, test: str, command: str, timeout: int) -> None:
    invocation = [
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
        test,
        "--build-mode",
        args.build_mode,
        "--cache-state",
        "storage-backup",
        "--timeout",
        str(timeout),
        "--command",
        command,
    ]
    if args.quiet:
        invocation.append("--quiet")
    subprocess.run(invocation, cwd=ROOT, check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--backup-id", required=True)
    parser.add_argument("--confirm-p3-start", required=True)
    parser.add_argument("--serial", default="/dev/cu.wchusbserial120")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--port", type=int, default=8009)
    parser.add_argument(
        "--build-mode",
        choices=("production", "diag_off", "diag_on"),
        default="production",
    )
    parser.add_argument("--timeout", type=int, default=3600)
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    try:
        confirmed_start = int(args.confirm_p3_start, 0)
    except ValueError as exc:
        parser.error("--confirm-p3-start must be an integer")
        raise AssertionError from exc
    if confirmed_start != EXPECTED_P3_START:
        parser.error(
            f"confirmation mismatch: {confirmed_start:#x} != {EXPECTED_P3_START:#x}"
        )
    if not BACKUP_ID.fullmatch(args.backup_id):
        parser.error("--backup-id contains unsafe characters")
    if not args.run_dir.is_dir():
        parser.error(f"run directory does not exist: {args.run_dir}")
    if not BOARD_SCRIPT.is_file():
        parser.error(f"board backup script is missing: {BOARD_SCRIPT}")

    script_sha = hashlib.sha256(BOARD_SCRIPT.read_bytes()).hexdigest()
    url = f"http://{args.host_ip}:{args.port}/{BOARD_SCRIPT.name}"
    target = f"/scratch/{BOARD_SCRIPT.name}"
    download = "/bin/busybox wget -O %s %s" % tuple(
        map(shlex.quote, (target, url))
    )
    verify = "printf '%s  %s\\n'|/bin/busybox sha256sum -c -" % (
        script_sha,
        shlex.quote(target),
    )
    phases = (
        (
            "p3_backup_deploy",
            download,
            120,
        ),
        (
            "p3_backup_script_sha256",
            verify,
            120,
        ),
        (
            "p3_backup_copy_verify",
            f"/bin/sh {shlex.quote(target)} {shlex.quote(args.backup_id)}",
            args.timeout,
        ),
        (
            "p3_backup_complete_gate",
            f"test -f /persist/backups/{args.backup_id}/COMPLETE&&"
            f"cat /persist/backups/{args.backup_id}/COMPLETE&&"
            f"cat /persist/backups/{args.backup_id}/MANIFEST.txt",
            120,
        ),
    )

    if args.dry_run:
        for name, command, timeout in phases:
            print(f"{name} timeout={timeout} command_bytes={len(command.encode())}")
            print(command)
        return 0

    server = subprocess.Popen(
        http_server_command(BOARD_SCRIPT.parent, args.host_ip, args.port),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.3)
    if server.poll() is not None:
        raise SystemExit("backup HTTP server failed to start")
    try:
        for name, command, timeout in phases:
            run_board(args, name, command, timeout)
    except subprocess.CalledProcessError as exc:
        return exc.returncode
    finally:
        server.terminate()
        try:
            server.wait(timeout=2)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=2)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
