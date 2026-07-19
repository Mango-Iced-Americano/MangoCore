#!/usr/bin/env python3
"""Deploy only the benchmark sources to an isolated board directory over HTTP."""

from __future__ import annotations

import argparse
import hashlib
import shlex
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PACKAGER = ROOT / "scripts" / "package_cpython_bench.py"
HARNESS = ROOT / "scripts" / "kernel_perf.py"
DEFAULT_BUNDLE = ROOT / "target" / "cpython-bench" / "cpython-bench-suite.zip"


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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--serial", default="/dev/cu.wchusbserial120")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--port", type=int, default=8008)
    parser.add_argument("--bundle", type=Path, default=DEFAULT_BUNDLE)
    parser.add_argument(
        "--target-dir",
        default="/scratch",
        help="existing writable parent on the board (for example /persist/mangocore-perf)",
    )
    parser.add_argument("--build-mode", choices=("production", "diag_off", "diag_on"), default="production")
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    if not args.run_dir.is_dir():
        parser.error("run directory does not exist: %s" % args.run_dir)
    target_dir = args.target_dir.rstrip("/") or "/"
    if target_dir == "/" or not target_dir.startswith(("/scratch", "/persist/")):
        parser.error("target directory must be below /scratch or /persist")
    package = subprocess.run(
        [sys.executable, str(PACKAGER), "--output", str(args.bundle)],
        cwd=ROOT,
        check=False,
    )
    if package.returncode:
        return package.returncode

    bundle_hash = hashlib.sha256(args.bundle.read_bytes()).hexdigest()
    url = "http://%s:%d/%s" % (args.host_ip, args.port, args.bundle.name)
    download = "/bin/busybox wget -O b.zip %s" % shlex.quote(url)
    extract_code = "import zipfile as z;z.ZipFile('b.zip').extractall()"
    verify = "printf '%s  b.zip\\n'|/bin/busybox sha256sum -c -" % bundle_hash
    phases = (
        (
            "cpybench_download",
            "mkdir -p %s&&cd %s&&rm -rf mangocore-cpython-bench-suite b.zip&&"
            % (shlex.quote(target_dir), shlex.quote(target_dir))
            + download,
        ),
        (
            "cpybench_sha256",
            "cd %s&&%s" % (shlex.quote(target_dir), verify),
        ),
        (
            "cpybench_extract",
            "cd %s&&/usr/bin/python3 -c %s&&sync"
            % (shlex.quote(target_dir), shlex.quote(extract_code)),
        ),
        (
            "cpybench_verify",
            "cd %s&&test -f mangocore-cpython-bench-suite/bench/bench_runner.py&&"
            % shlex.quote(target_dir)
            +
            "/bin/sh mangocore-cpython-bench-suite/cpython_benchmark.sh --list",
        ),
    )
    if args.dry_run:
        for name, board_command in phases:
            print("%s command_bytes=%d" % (name, len(board_command.encode("utf-8"))))
            print(board_command)
        return 0

    server = subprocess.Popen(
        http_server_command(args.bundle.resolve().parent, args.host_ip, args.port),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.3)
    if server.poll() is not None:
        raise SystemExit("benchmark HTTP server failed to start")
    try:
        for test_name, board_command in phases:
            command = [
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
                test_name,
                "--build-mode",
                args.build_mode,
                "--cache-state",
                "deployment",
                "--timeout",
                "120",
                "--command",
                board_command,
            ]
            if args.quiet:
                command.append("--quiet")
            result = subprocess.run(command, cwd=ROOT, check=False)
            if result.returncode:
                return result.returncode
        return 0
    finally:
        server.terminate()
        try:
            server.wait(timeout=2)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=2)


if __name__ == "__main__":
    raise SystemExit(main())
