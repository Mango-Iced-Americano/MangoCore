#!/usr/bin/env python3
"""Prepare a macOS TFTP link and boot MangoCore through 2K1000LA U-Boot."""

from __future__ import annotations

import argparse
import glob
import hashlib
import os
from pathlib import Path
import re
import select
import shutil
import subprocess
import sys
import termios
import time
import tty
from typing import Optional
import zlib


DEFAULT_TFTP_ROOT = Path("/private/tftpboot")
TFTP_PLIST = Path("/System/Library/LaunchDaemons/tftp.plist")
TFTP_SERVICE = "system/com.apple.tftpd"


class BootError(RuntimeError):
    pass


def run(command: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, check=check, text=True, capture_output=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while data := source.read(4 * 1024 * 1024):
            digest.update(data)
    return digest.hexdigest()


def crc32(path: Path) -> int:
    checksum = 0
    with path.open("rb") as source:
        while data := source.read(4 * 1024 * 1024):
            checksum = zlib.crc32(data, checksum)
    return checksum


def sudo(command: list[str]) -> None:
    print("+ sudo", " ".join(command), flush=True)
    subprocess.run(["sudo", *command], check=True)


def ensure_interface(interface: str, host_ip: str, netmask: str, configure: bool) -> None:
    result = run(["ifconfig", interface], check=False)
    if result.returncode != 0:
        raise BootError(f"network interface not found: {interface}")
    current = result.stdout
    if re.search(rf"\binet\s+{re.escape(host_ip)}\b", current):
        print(f"[host] {interface} already uses {host_ip}/{netmask}")
    elif configure:
        sudo(["ifconfig", interface, "inet", host_ip, "netmask", netmask, "up"])
        current = run(["ifconfig", interface]).stdout
        if not re.search(rf"\binet\s+{re.escape(host_ip)}\b", current):
            raise BootError(f"failed to configure {host_ip} on {interface}")
        print(f"[host] configured {interface} as {host_ip}/{netmask}")
    else:
        raise BootError(
            f"{interface} does not have {host_ip}; rerun without --no-host-config"
        )
    if "status: active" not in current:
        raise BootError(f"{interface} has no active Ethernet link")


def ensure_tftp_service(configure: bool) -> None:
    result = run(["launchctl", "print", TFTP_SERVICE], check=False)
    if result.returncode == 0:
        print(f"[host] TFTP service ready: {TFTP_SERVICE}")
        return
    if not configure:
        raise BootError("macOS TFTP service is not loaded")
    if not TFTP_PLIST.is_file():
        raise BootError(f"TFTP launch daemon plist not found: {TFTP_PLIST}")
    sudo(["launchctl", "load", "-F", str(TFTP_PLIST)])
    if run(["launchctl", "print", TFTP_SERVICE], check=False).returncode != 0:
        raise BootError("failed to load the macOS TFTP service")
    print(f"[host] loaded TFTP service: {TFTP_SERVICE}")


def prepare_tftp_image(image: Path, tftp_root: Path) -> Path:
    image = image.expanduser().resolve()
    if not image.is_file():
        raise BootError(f"kernel image not found: {image}")
    if image.stat().st_size < 64:
        raise BootError(f"kernel image is too small to be a uImage: {image}")

    if not tftp_root.exists():
        try:
            tftp_root.mkdir(parents=True)
        except PermissionError:
            sudo(["mkdir", "-p", str(tftp_root)])
    destination = (tftp_root / image.name).resolve()
    source_hash = sha256(image)
    if destination != image and (
        not destination.is_file() or sha256(destination) != source_hash
    ):
        try:
            shutil.copy2(image, destination)
        except PermissionError:
            sudo(["cp", "-f", str(image), str(destination)])
    if not destination.is_file() or sha256(destination) != source_hash:
        raise BootError("TFTP image copy verification failed")
    try:
        destination.chmod(0o644)
    except PermissionError:
        sudo(["chmod", "0644", str(destination)])
    print(
        f"[host] TFTP image ready: {destination} "
        f"({destination.stat().st_size} bytes, sha256={source_hash})"
    )
    return destination


def detect_serial(explicit: Optional[str]) -> str:
    if explicit:
        path = str(Path(explicit).expanduser())
        if not Path(path).exists():
            raise BootError(f"serial device not found: {path}")
        return path
    candidates = sorted(glob.glob("/dev/cu.wchusbserial*"))
    if len(candidates) != 1:
        raise BootError(
            "expected exactly one CH340 serial device; "
            f"found {candidates or 'none'} (use --serial)"
        )
    return candidates[0]


def release_matching_screen(serial_path: str, takeover: bool) -> None:
    processes = run(["ps", "-ax", "-o", "pid=,command="], check=False).stdout
    owners: list[str] = []
    for line in processes.splitlines():
        if serial_path not in line or "SCREEN" not in line:
            continue
        fields = line.strip().split(None, 1)
        if fields:
            owners.append(fields[0])
    if not owners:
        return
    if not takeover:
        raise BootError(
            f"serial device is held by screen PID(s) {owners}; close screen or allow takeover"
        )
    sessions = run(["screen", "-ls"], check=False).stdout
    for pid in owners:
        match = re.search(rf"^\s*({re.escape(pid)}\.[^\s]+)", sessions, re.MULTILINE)
        if match is None:
            raise BootError(f"could not resolve screen session for PID {pid}")
        session = match.group(1)
        print(f"[host] closing screen session that owns serial: {session}")
        subprocess.run(["screen", "-S", session, "-X", "quit"], check=True)
    time.sleep(0.5)


class UBootConsole:
    def __init__(self, serial_module, port: str, log_path: Path) -> None:
        log_path.parent.mkdir(parents=True, exist_ok=True)
        self.log = log_path.open("ab", buffering=0)
        try:
            self.serial = serial_module.Serial(
                port,
                115200,
                timeout=0.2,
                write_timeout=2,
                exclusive=True,
            )
        except serial_module.SerialException as error:
            self.log.close()
            raise BootError(f"cannot open serial device {port}: {error}") from error
        self.serial.dtr = False
        self.serial.rts = False

    def close(self) -> None:
        self.serial.close()
        self.log.close()

    def _record(self, data: bytes) -> None:
        self.log.write(data)

    def _read_prompt(self, timeout: float) -> str:
        deadline = time.monotonic() + timeout
        output = bytearray()
        while time.monotonic() < deadline:
            data = self.serial.read(self.serial.in_waiting or 1)
            if not data:
                continue
            output.extend(data)
            self._record(data)
            if output.endswith(b"=> "):
                return output.decode("utf-8", errors="replace")
        tail = output[-1000:].decode("utf-8", errors="replace")
        raise BootError(f"timeout waiting for U-Boot prompt; tail:\n{tail}")

    def acquire_prompt(self, timeout: float) -> None:
        self.serial.reset_input_buffer()
        self.serial.write(b"\r")
        self.serial.flush()
        try:
            self._read_prompt(1.5)
            print("[uboot] prompt already available")
            return
        except BootError:
            pass

        print(
            "[uboot] press the board RESET button now; waiting and sending 'c' "
            "to stop autoboot...",
            flush=True,
        )
        deadline = time.monotonic() + timeout
        output = bytearray()
        next_interrupt = 0.0
        while time.monotonic() < deadline:
            now = time.monotonic()
            if now >= next_interrupt:
                self.serial.write(b"c")
                self.serial.flush()
                next_interrupt = now + 0.2
            data = self.serial.read(self.serial.in_waiting or 1)
            if not data:
                continue
            output.extend(data)
            self._record(data)
            if output.endswith(b"=> "):
                # Clear any extra 'c' that raced with the prompt before the
                # first real command is sent.
                self.serial.write(b"\x03\r")
                self.serial.flush()
                self._read_prompt(3)
                print("[uboot] autoboot interrupted")
                return
            if len(output) > 64 * 1024:
                del output[:-4096]
        raise BootError("U-Boot prompt not reached; RESET was not observed")

    def command(self, command: str, timeout: float = 15) -> str:
        self.log.write(f"\n>>> {command}\n".encode("ascii"))
        self.serial.write(command.encode("ascii") + b"\r")
        self.serial.flush()
        output = self._read_prompt(timeout)
        if "Unknown command" in output:
            raise BootError(f"U-Boot does not support command: {command}")
        return output

    def boot_and_stream(self, loadaddr: str) -> None:
        command = f"bootm {loadaddr}"
        self.log.write(f"\n>>> {command}\n".encode("ascii"))
        self.serial.write(command.encode("ascii") + b"\r")
        self.serial.flush()
        print("[console] booting; Ctrl-C exits this monitor only", flush=True)

        stdin_fd = sys.stdin.fileno()
        serial_fd = self.serial.fileno()
        saved_terminal = None
        if sys.stdin.isatty():
            saved_terminal = termios.tcgetattr(stdin_fd)
            tty.setraw(stdin_fd)

        try:
            while True:
                readable, _, _ = select.select([serial_fd, stdin_fd], [], [], 0.2)
                if serial_fd in readable:
                    data = self.serial.read(self.serial.in_waiting or 1)
                    if data:
                        self._record(data)
                        sys.stdout.buffer.write(data)
                        sys.stdout.buffer.flush()

                if stdin_fd in readable:
                    data = os.read(stdin_fd, 1024)
                    if not data:
                        return
                    # Keep Ctrl-C local to the monitor. Bytes typed before it
                    # still reach the board, while the board never receives
                    # an accidental interrupt character.
                    before_escape, escape, _ = data.partition(b"\x03")
                    if escape:
                        if before_escape:
                            self.serial.write(before_escape)
                            self.serial.flush()
                        return
                    self.serial.write(data)
                    self.serial.flush()
        finally:
            if saved_terminal is not None:
                termios.tcsetattr(stdin_fd, termios.TCSADRAIN, saved_terminal)
            print("\n[console] monitor closed; board continues running")


def require(pattern: str, output: str, message: str) -> re.Match[str]:
    match = re.search(pattern, output, re.IGNORECASE | re.MULTILINE)
    if match is None:
        raise BootError(f"{message}; U-Boot output:\n{output[-2000:]}")
    return match


def boot(args, serial_module, tftp_image: Path, serial_path: str) -> None:
    release_matching_screen(serial_path, args.takeover_screen)
    console = UBootConsole(serial_module, serial_path, args.log)
    try:
        console.acquire_prompt(args.reset_timeout)
        commands = (
            f"setenv ipaddr {args.board_ip}",
            f"setenv serverip {args.host_ip}",
            f"setenv netmask {args.netmask}",
            f"setenv loadaddr {args.loadaddr}",
        )
        for command in commands:
            console.command(command)

        print(f"[uboot] ping {args.host_ip}", flush=True)
        ping = console.command(f"ping {args.host_ip}", timeout=20)
        require(rf"host\s+{re.escape(args.host_ip)}\s+is\s+alive", ping, "ping failed")

        print(f"[uboot] tftpboot {tftp_image.name}", flush=True)
        transfer = console.command(
            f"tftpboot {args.loadaddr} {tftp_image.name}", timeout=args.tftp_timeout
        )
        transferred = int(
            require(r"Bytes transferred\s*=\s*(\d+)", transfer, "TFTP failed").group(1)
        )
        expected_size = tftp_image.stat().st_size
        if transferred != expected_size:
            raise BootError(f"short TFTP transfer: {transferred} != {expected_size}")

        expected_crc = crc32(tftp_image)
        checksum = console.command(f"crc32 {args.loadaddr} 0x{expected_size:x}", timeout=60)
        actual_crc = int(
            require(r"==>\s*([0-9a-f]{8})", checksum, "U-Boot CRC missing").group(1),
            16,
        )
        if actual_crc != expected_crc:
            raise BootError(f"TFTP CRC mismatch: {actual_crc:08x} != {expected_crc:08x}")
        print(f"[uboot] transfer verified: {transferred} bytes, crc32={actual_crc:08x}")

        image_info = console.command(f"iminfo {args.loadaddr}", timeout=30)
        require(r"LoongArch", image_info, "uImage architecture check failed")
        require(r"Checksum\s+\.\.\.\s+OK", image_info, "uImage checksum failed")
        print("[uboot] iminfo verified: LoongArch, checksum OK")

        if args.no_boot:
            print("[uboot] --no-boot selected; leaving board at the U-Boot prompt")
            return
        console.boot_and_stream(args.loadaddr)
    except KeyboardInterrupt:
        print("\n[console] monitor closed; board continues running")
    finally:
        console.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="One-command macOS TFTP boot for the Loongson 2K1000LA board"
    )
    parser.add_argument("--interface", default="en8")
    parser.add_argument("--serial")
    parser.add_argument(
        "--image",
        type=Path,
        required=True,
        help="uImage path or filename to copy into the TFTP root and boot",
    )
    parser.add_argument("--tftp-root", type=Path, default=DEFAULT_TFTP_ROOT)
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--board-ip", default="192.168.9.20")
    parser.add_argument("--netmask", default="255.255.255.0")
    parser.add_argument("--loadaddr", default="0x9000000098000000")
    parser.add_argument("--log", type=Path, default=Path("/private/tmp/mango-2k1000-boot.log"))
    parser.add_argument("--reset-timeout", type=float, default=120)
    parser.add_argument("--tftp-timeout", type=float, default=180)
    parser.add_argument("--no-host-config", dest="configure_host", action="store_false")
    parser.add_argument("--no-screen-takeover", dest="takeover_screen", action="store_false")
    parser.add_argument("--check-only", action="store_true")
    parser.add_argument("--no-boot", action="store_true")
    parser.set_defaults(configure_host=True, takeover_screen=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if sys.platform != "darwin":
        raise BootError("this host setup workflow currently supports macOS only")
    try:
        import serial
    except ImportError as error:
        raise BootError("pyserial is required: python3 -m pip install --user pyserial") from error

    ensure_interface(args.interface, args.host_ip, args.netmask, args.configure_host)
    ensure_tftp_service(args.configure_host)
    tftp_image = prepare_tftp_image(args.image, args.tftp_root)
    serial_path = detect_serial(args.serial)
    print(f"[host] serial device: {serial_path}")
    if args.check_only:
        print("[host] check complete; U-Boot commands were not sent")
        return
    boot(args, serial, tftp_image, serial_path)


if __name__ == "__main__":
    try:
        main()
    except (BootError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
