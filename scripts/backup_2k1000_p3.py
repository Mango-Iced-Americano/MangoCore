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
import uuid

from boot_2k1000_tftp import ensure_interface


ROOT = Path(__file__).resolve().parents[1]
BOARD_SCRIPT = ROOT / "scripts" / "board" / "backup_2k1000_p3.sh"
PREP_BOARD_SCRIPT = ROOT / "scripts" / "board" / "prepare_2k1000_p3_backup.sh"
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
    """Run one bounded shell transaction on the board console.

    The persistent P3 image's UART TTY accepts CR as the reliable line
    terminator; the generic performance harness deliberately uses LF for
    QEMU compatibility and therefore cannot drive this recovery shell.
    """
    try:
        import serial
    except ImportError as error:
        raise SystemExit("pyserial is required") from error

    nonce = uuid.uuid4().hex[:8]
    end = f"__MANGO_BACKUP_{test}_RC="
    wrapped = f"{command}; echo {end}$?"
    if len(wrapped.encode("utf-8")) > 2048:
        raise SystemExit(f"board command is too long: {test}")

    log_path = args.run_dir / "raw" / f"{test}-{nonce}.log"
    deadline = time.monotonic() + timeout
    pending = b""
    with serial.Serial(args.serial, args.baud, timeout=0.1, write_timeout=2) as port, log_path.open(
        "wb"
    ) as log:
        port.dtr = False
        port.rts = False
        port.reset_input_buffer()
        for byte in wrapped.encode("utf-8") + b"\r":
            port.write(bytes((byte,)))
            port.flush()
            # 2K1000's current UART path can only sustain interactive-speed
            # input while GMAC polling is live.  Faster input corrupts shell
            # tokens and is never treated as a successful transaction.
            time.sleep(0.2)
        while time.monotonic() < deadline:
            data = port.read(port.in_waiting or 1)
            if not data:
                continue
            log.write(data)
            log.flush()
            pending += data
            while b"\n" in pending:
                raw, pending = pending.split(b"\n", 1)
                line = raw.decode("utf-8", errors="replace").rstrip("\r")
                if not args.quiet:
                    print(line, flush=True)
                match = re.search(re.escape(end) + r"(-?\d+)", line)
                if match:
                    rc = int(match.group(1))
                    if rc:
                        raise SystemExit(f"board command failed: {test} rc={rc}; log={log_path}")
                    print(f"[board] {test}: PASS", flush=True)
                    return
    raise SystemExit(f"board command timed out: {test}; log={log_path}")


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
    # kernel_perf board records each serial transaction below raw/.
    (args.run_dir / "raw").mkdir(exist_ok=True)
    for script in (BOARD_SCRIPT, PREP_BOARD_SCRIPT):
        if not script.is_file():
            parser.error(f"board backup script is missing: {script}")

    script_sha = hashlib.sha256(BOARD_SCRIPT.read_bytes()).hexdigest()
    url = f"http://{args.host_ip}:{args.port}/{BOARD_SCRIPT.name}"
    prep_url = f"http://{args.host_ip}:{args.port}/{PREP_BOARD_SCRIPT.name}"
    target = f"/persist/.{BOARD_SCRIPT.name}"
    prepare_target = "/tmp/mango-p3-backup-prepare.sh"
    prepare = "/bin/busybox wget -O %s %s&&/bin/sh %s" % tuple(
        map(shlex.quote, (prepare_target, prep_url, prepare_target))
    )
    download = "/bin/busybox wget -O %s %s" % tuple(
        map(shlex.quote, (target, url))
    )
    verify = "printf '%s  %s\\n'|/bin/busybox sha256sum -c -" % (
        script_sha,
        shlex.quote(target),
    )
    phases = (
        (
            "p3_backup_prepare_readonly_source",
            prepare,
            120,
        ),
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

    # The backup script is fetched over the board-only Ethernet link.  Keep
    # this setup in the same guarded entrypoint as the serial operation so a
    # stale developer LAN address cannot turn into a confusing HTTP failure.
    ensure_interface("en8", args.host_ip, "255.255.255.0", True)

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
