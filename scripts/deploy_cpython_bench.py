#!/usr/bin/env python3
"""Deploy only the benchmark sources to an isolated board directory over HTTP."""

from __future__ import annotations

import argparse
import functools
import hashlib
import http.server
import shlex
import subprocess
import sys
import threading
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PACKAGER = ROOT / "scripts" / "package_cpython_bench.py"
HARNESS = ROOT / "scripts" / "kernel_perf.py"
DEFAULT_BUNDLE = ROOT / "target" / "cpython-bench" / "cpython-bench-suite.zip"


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: object) -> None:
        return


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
    download_code = (
        "import urllib.request as u,zipfile as z;"
        "u.urlretrieve(%r,'b.zip');z.ZipFile('b.zip').extractall()" % url
    )
    verify_code = (
        "import hashlib as h;"
        "assert h.sha256(open('b.zip','rb').read()).hexdigest()==%r" % bundle_hash
    )
    phases = (
        (
            "cpybench_deploy",
            "mkdir -p %s&&cd %s&&rm -rf mangocore-cpython-bench-suite b.zip&&"
            % (shlex.quote(target_dir), shlex.quote(target_dir))
            +
            "/tools/tests/cpython/python3-wrapper.sh -c %s&&sync" % shlex.quote(download_code),
        ),
        (
            "cpybench_sha256",
            "cd %s&&/tools/tests/cpython/python3-wrapper.sh -c %s"
            % (shlex.quote(target_dir), shlex.quote(verify_code)),
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

    handler = functools.partial(QuietHandler, directory=str(args.bundle.resolve().parent))
    server = http.server.ThreadingHTTPServer((args.host_ip, args.port), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
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
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


if __name__ == "__main__":
    raise SystemExit(main())
