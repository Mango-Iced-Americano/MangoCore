#!/usr/bin/env python3
"""Automated VF2 TFTP boot with serial- or DTR-based board reset.

Usage:
  python3 scripts/boot_vf2_tftp.py                        # build + deploy + boot
  python3 scripts/boot_vf2_tftp.py --no-build              # skip build, boot existing
  python3 scripts/boot_vf2_tftp.py --reset-only            # just reset the board
  python3 scripts/boot_vf2_tftp.py --serial /dev/ttyUSB0   # explicit serial device
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import select
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

import serial  # pyserial


# ── Paths ────────────────────────────────────────────────────────────
WORKSPACE = Path(__file__).resolve().parent.parent
KERNEL_ELF = (
    WORKSPACE / "build" / "rv64" / "release" / "normal" / "kernel"
    / "riscv64gc-unknown-none-elf" / "release" / "os"
)
TFTP_ROOT = Path("/srv/tftp/vf2")
TFTP_IMAGE_NAME = "kernel-rv.elf"

# ── Network ──────────────────────────────────────────────────────────
HOST_IP = "192.168.200.1"
BOARD_IP = "192.168.200.10"
NETMASK = "255.255.255.0"
VID = "0x40200000"
LOADADDR = "0x60000000"

# ── Serial ───────────────────────────────────────────────────────────
SERIAL_CANDIDATES = [
    "/dev/ttyUSB0", "/dev/ttyUSB1", "/dev/ttyUSB2",
    "/dev/ttyACM0", "/dev/ttyACM1",
]
BAUDRATE = 115200


class BootError(RuntimeError):
    pass


# ────────────────────────────────────────────────────────────────────
#  Host helpers
# ────────────────────────────────────────────────────────────────────

def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while data := source.read(4 * 1024 * 1024):
            digest.update(data)
    return digest.hexdigest()


def detect_serial(explicit: Optional[str]) -> str:
    if explicit:
        if not Path(explicit).exists():
            raise BootError(f"serial device not found: {explicit}")
        return explicit
    for candidate in SERIAL_CANDIDATES:
        if Path(candidate).exists():
            return candidate
    raise BootError(
        f"no serial device found (checked: {SERIAL_CANDIDATES}). "
        "Use --serial to specify"
    )


# ────────────────────────────────────────────────────────────────────
#  Build & Deploy
# ────────────────────────────────────────────────────────────────────

def build_kernel(cmdline: str) -> None:
    """Build the kernel via the project's Makefile facade inside Docker."""
    print(f"[build] command line: {cmdline}")
    result = subprocess.run(
        [
            "docker", "compose", "exec", "-T", "os-dev",
            "make", "kernel",
            "ARCH=rv64", "PROFILE=normal", "BOARD=vf2", "BLK_MODE=virt",
            f"KERNEL_CMDLINE={cmdline}",
        ],
        check=True,
        timeout=300,
        text=True,
        capture_output=True,
        cwd=str(WORKSPACE),
    )
    last = result.stderr.strip().split("\n")[-1] if result.stderr.strip() else ""
    print(f"[build] {last}")


def deploy_kernel() -> Path:
    """Copy the kernel ELF to the TFTP directory."""
    if not KERNEL_ELF.is_file():
        raise BootError(f"kernel ELF not found: {KERNEL_ELF}")
    dest = TFTP_ROOT / TFTP_IMAGE_NAME
    TFTP_ROOT.mkdir(parents=True, exist_ok=True)
    shutil.copy2(KERNEL_ELF, dest)
    size = dest.stat().st_size
    print(f"[deploy] {dest} ({size} bytes, sha256={sha256(dest)[:16]}…)")
    return dest


# ────────────────────────────────────────────────────────────────────
#  U-Boot serial console
# ────────────────────────────────────────────────────────────────────

class UBootConsole:
    def __init__(self, port: str) -> None:
        try:
            self.ser = serial.Serial(
                port, BAUDRATE, timeout=0.2, write_timeout=2, exclusive=True,
            )
        except serial.SerialException as e:
            raise BootError(f"cannot open {port}: {e}") from e
        self.port = port

    def close(self) -> None:
        self.ser.close()

    def _write(self, data: bytes) -> None:
        for offset in range(0, len(data)):
            self.ser.write(data[offset:offset+1])
            self.ser.flush()
            time.sleep(0.004)

    def _read_until(self, pattern: bytes, timeout: float) -> str:
        deadline = time.monotonic() + timeout
        buf = bytearray()
        while time.monotonic() < deadline:
            data = self.ser.read(self.ser.in_waiting or 1)
            if data:
                buf.extend(data)
                if pattern in buf:
                    return buf.decode("utf-8", errors="replace")
        tail = buf[-1000:].decode("utf-8", errors="replace") if buf else "(no output)"
        raise BootError(f"timeout waiting for {pattern!r}; tail:\n{tail}")

    def send_line(self, line: str) -> None:
        sys.stdout.write(f">>> {line}\n")
        sys.stdout.flush()
        self._write(line.encode("ascii") + b"\r")

    def wait_prompt(self, timeout: float = 5) -> None:
        self.ser.reset_input_buffer()
        self._write(b"\r")
        try:
            self._read_until(b"StarFive #", timeout)
            print("[uboot] prompt ready")
        except BootError:
            raise BootError("U-Boot prompt not found — is the board powered on?")

    def command(self, cmd: str, timeout: float = 15) -> str:
        self.send_line(cmd)
        output = self._read_until(b"StarFive #", timeout)
        if "Unknown command" in output:
            raise BootError(f"U-Boot does not support: {cmd}")
        return output

    def reset_board(self) -> None:
        """Reset via U-Boot 'reset' command."""
        print("[uboot] sending reset…")
        self._write(b"reset\r")
        self.ser.flush()
        time.sleep(1)
        # Board will reboot; wait for U-Boot to reappear.
        print("[uboot] waiting for board to reboot…")
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            data = self.ser.read(self.ser.in_waiting or 1)
            if data and b"StarFive #" in data:
                # Clear any stale input
                self._write(b"\x03\r")
                time.sleep(0.5)
                self.wait_prompt()
                print("[uboot] board rebooted")
                return
        raise BootError("board did not reboot within 30s")

    def dtr_reset(self) -> None:
        """Hardware reset via DTR toggle (CH340/CP2102/FTDI)."""
        print("[serial] DTR toggle reset…")
        # Most adapters: DTR low → RESET asserted, DTR high → release
        self.ser.dtr = False
        time.sleep(0.1)
        self.ser.dtr = True
        time.sleep(0.5)
        print("[serial] DTR pulse sent; waiting for U-Boot…")
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            data = self.ser.read(self.ser.in_waiting or 1)
            if data and b"StarFive #" in data:
                self._write(b"\x03\r")
                time.sleep(0.5)
                self.wait_prompt()
                print("[uboot] board reset via DTR")
                return
        raise BootError("DTR reset: board did not respond within 30s")

    def reset(self) -> None:
        """Try software reset first, fall back to DTR hardware reset."""
        # First try: are we already at U-Boot?
        try:
            self.wait_prompt(timeout=2)
            self.reset_board()
            return
        except BootError:
            pass

        # Second try: DTR hardware reset
        try:
            self.dtr_reset()
            return
        except BootError:
            pass

        raise BootError(
            "all reset methods failed. Manually press the board RESET button "
            "and re-run with --reset-only"
        )


# ────────────────────────────────────────────────────────────────────
#  Boot flow
# ────────────────────────────────────────────────────────────────────

def boot_flow(console: UBootConsole, tftp_name: str) -> None:
    # Configure network
    console.command(f"setenv ipaddr {BOARD_IP}")
    console.command(f"setenv serverip {HOST_IP}")
    console.command(f"setenv netmask {NETMASK}")
    console.command(f"setenv loadaddr {LOADADDR}")

    # Ping host
    print(f"[uboot] ping {HOST_IP}…")
    result = console.command(f"ping {HOST_IP}", timeout=20)
    if f"host {HOST_IP} is alive" not in result.lower():
        raise BootError(f"ping to {HOST_IP} failed")

    # TFTP download with explicit server IP (Lupyuen workaround for JH7110 unicast issue)
    tftp_uri = f"{HOST_IP}:{tftp_name}"
    print(f"[uboot] tftpboot {tftp_uri}…")
    result = console.command(f"tftpboot {LOADADDR} {tftp_uri}", timeout=60)
    match = re.search(r"Bytes transferred\s*=\s*(\d+)", result)
    if not match:
        raise BootError(f"TFTP failed; output:\n{result[-500:]}")
    print(f"[uboot] transferred {match.group(1)} bytes")

    # Boot ELF — load to 0x60000000, bootelf places segments at link address 0x40200000
    print("[uboot] bootelf…")
    console.send_line(f"bootelf {LOADADDR}")

    # Stream serial output until the board reboots (ktest mode) or user interrupts
    ktest_pattern = re.compile(rb"\[KTEST RESULT: (PASS|FAIL)\]")
    print("[console] streaming; Ctrl-C to return to host…")
    try:
        while True:
            data = console.ser.read(console.ser.in_waiting or 1)
            if data:
                sys.stdout.buffer.write(data)
                sys.stdout.buffer.flush()
                if ktest_pattern.search(data):
                    break
            else:
                time.sleep(0.05)
    except KeyboardInterrupt:
        print("\n[console] interrupted; board continues running")


def boot_flow_loop(console: UBootConsole, tftp_name: str, max_iterations: int) -> None:
    """Repeatedly boot, wait for ktest result, reset, and boot again."""
    ktest_pattern = re.compile(rb"\[KTEST RESULT: (PASS|FAIL)\]")
    iterations = 0

    while max_iterations == 0 or iterations < max_iterations:
        iterations += 1
        print(f"\n{'='*60}")
        print(f"[loop] iteration {iterations}")

        # Configure U-Boot network
        console.command(f"setenv ipaddr {BOARD_IP}")
        console.command(f"setenv serverip {HOST_IP}")
        console.command(f"setenv netmask {NETMASK}")

        # TFTP download with explicit server IP
        tftp_uri = f"{HOST_IP}:{tftp_name}"
        print(f"[uboot] tftpboot {tftp_uri}…")
        result = console.command(f"tftpboot {LOADADDR} {tftp_uri}", timeout=60)
        match = re.search(r"Bytes transferred\s*=\s*(\d+)", result)
        if not match:
            raise BootError(f"TFTP failed; output:\n{result[-500:]}")
        print(f"[uboot] transferred {match.group(1)} bytes")

        # Boot ELF
        print("[uboot] bootelf…")
        console.send_line(f"bootelf {LOADADDR}")

        # Monitor serial for ktest result
        print(f"[loop] waiting for ktest result…")
        buf = bytearray()
        _deadline = time.monotonic() + 120  # 2 min timeout
        result_seen = None
        while time.monotonic() < _deadline:
            data = console.ser.read(console.ser.in_waiting or 1)
            if data:
                sys.stdout.buffer.write(data)
                sys.stdout.buffer.flush()
                buf.extend(data)
                m = ktest_pattern.search(buf)
                if m:
                    result_seen = m.group(1).decode()
                    break
                # Keep buffer bounded
                if len(buf) > 128 * 1024:
                    del buf[:-16384]
            else:
                time.sleep(0.05)

        if result_seen:
            print(f"\n[loop] ktest result: {result_seen}")
        else:
            print(f"\n[loop] ktest timed out — no result marker seen")

        # Wait a bit for shutdown to complete, then reset
        time.sleep(1)
        print("[loop] resetting board…")
        console.reset()


def cmdline_ktest(ktest: str) -> str:
    return f"mango.mode=ktest mango.test={ktest}"


def cmdline_normal() -> str:
    return "mango.mode=normal"


# ────────────────────────────────────────────────────────────────────
#  CLI
# ────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(description="Automated VF2 TFTP boot")
    parser.add_argument("--serial", help="Serial device path (auto-detect if omitted)")
    parser.add_argument("--no-build", action="store_true", help="Skip Docker build, use existing ELF")
    parser.add_argument("--reset-only", action="store_true", help="Only reset the board, don't build or boot")
    parser.add_argument("--ktest", help="Ktest group to run (e.g. 'gmac', 'all')")
    parser.add_argument("--normal", action="store_true", help="Normal boot (not ktest)")
    parser.add_argument("--tftp-name", default=TFTP_IMAGE_NAME)
    parser.add_argument("--loop", type=int, nargs="?", const=0, default=None,
                        help="Continuous loop mode: reset, boot, monitor ktest result, repeat. "
                             "Optional max iterations (0=forever).")
    args = parser.parse_args()

    serial_path = detect_serial(args.serial)

    if args.reset_only:
        console = UBootConsole(serial_path)
        try:
            console.reset()
            print("[done] board reset successfully")
        finally:
            console.close()
        return

    # Determine command line
    if args.normal:
        cmdline = cmdline_normal()
    elif args.ktest:
        cmdline = cmdline_ktest(args.ktest)
    else:
        # Default: ktest mode with 'gmac'
        cmdline = cmdline_ktest("gmac")

    # Build
    if not args.no_build:
        build_kernel(cmdline)
    deploy_kernel()

    # Boot
    console = UBootConsole(serial_path)
    try:
        console.reset()
        if args.loop is not None:
            boot_flow_loop(console, args.tftp_name, args.loop)
        else:
            boot_flow(console, args.tftp_name)
    finally:
        console.close()


if __name__ == "__main__":
    try:
        main()
    except BootError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        raise SystemExit(1)
    except KeyboardInterrupt:
        print("\n[aborted]")
        raise SystemExit(1)
