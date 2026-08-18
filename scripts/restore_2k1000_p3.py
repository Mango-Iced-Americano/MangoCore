#!/usr/bin/env python3
"""Restore 2K1000LA P3 from a completed backup stored on P4."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import subprocess
import sys

from boot_2k1000_tftp import (
    BootError,
    DEFAULT_TFTP_ROOT,
    UBootConsole,
    detect_serial,
    ensure_interface,
    release_matching_screen,
    require,
)

P3_START_LBA = 0xA80800
P3_SECTORS = 0x180000
CHUNK_SECTORS = 0x20000
CHUNK_BYTES = CHUNK_SECTORS * 512
CHUNK_COUNT = P3_SECTORS // CHUNK_SECTORS
LOADADDR = "0x9000000098000000"
EXPECTED_MODEL = "TS32GMTS400"
BACKUP_ID_RE = re.compile(r"^[A-Za-z0-9._-]+$")


def crc(console: UBootConsole) -> str:
    output = console.command(f"crc32 {LOADADDR} 0x{CHUNK_BYTES:x}", timeout=90)
    return require(r"==>\s*([0-9a-f]{8})", output, "U-Boot CRC missing").group(1)


def restore_chunk(console: UBootConsole, backup_id: str, index: int) -> None:
    path = f"/backups/{backup_id}/p3-{index:02d}.bin"
    loaded = console.command(f"ext4load scsi 0:4 {LOADADDR} {path}", timeout=180)
    size = int(require(r"(\d+)\s+bytes read", loaded, f"backup chunk missing: {path}").group(1))
    if size != CHUNK_BYTES:
        raise BootError(f"unsafe backup chunk size for {path}: {size}")
    source_crc = crc(console)
    start = P3_START_LBA + index * CHUNK_SECTORS
    write = console.command(
        f"scsi write {LOADADDR} {start:#x} {CHUNK_SECTORS:#x}", timeout=240
    )
    require(rf"{CHUNK_SECTORS}\s+blocks written:\s+OK", write, f"P3 write failed at {start:#x}")
    read = console.command(
        f"scsi read {LOADADDR} {start:#x} {CHUNK_SECTORS:#x}", timeout=240
    )
    require(rf"{CHUNK_SECTORS}\s+blocks read:\s+OK", read, f"P3 readback failed at {start:#x}")
    actual_crc = crc(console)
    if actual_crc != source_crc:
        raise BootError(f"P3 CRC mismatch at {start:#x}: {actual_crc} != {source_crc}")
    print(f"[restore] chunk={index:02d} lba={start:#x} crc32={actual_crc}", flush=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial")
    parser.add_argument("--interface", default="en8")
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--board-ip", default="192.168.9.20")
    parser.add_argument("--netmask", default="255.255.255.0")
    parser.add_argument("--reset-timeout", type=float, default=120)
    parser.add_argument("--log", type=Path, default=Path("/private/tmp/mango-p3-restore.log"))
    parser.add_argument("--no-host-config", dest="configure_host", action="store_false")
    parser.add_argument("--backup-id", required=True)
    parser.add_argument("--confirm-p3-start", required=True)
    parser.set_defaults(configure_host=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if int(args.confirm_p3_start, 0) != P3_START_LBA:
        raise BootError(f"confirmation mismatch: expected {P3_START_LBA:#x}")
    if not BACKUP_ID_RE.fullmatch(args.backup_id) or args.backup_id in {".", ".."}:
        raise BootError("unsafe backup id")
    try:
        import serial
    except ImportError as error:
        raise BootError("pyserial is required") from error
    ensure_interface(args.interface, args.host_ip, args.netmask, args.configure_host)
    serial_path = detect_serial(args.serial)
    release_matching_screen(serial_path, True)
    console = UBootConsole(serial, serial_path, args.log)
    try:
        console.acquire_prompt(args.reset_timeout)
        for command in (
            f"setenv ipaddr {args.board_ip}",
            f"setenv serverip {args.host_ip}",
            f"setenv netmask {args.netmask}",
            f"setenv loadaddr {LOADADDR}",
        ):
            console.command(command)
        scsi = console.command("scsi reset", timeout=30)
        require(EXPECTED_MODEL, scsi, "unexpected or missing SSD")
        console.command("scsi part 0", timeout=20)
        marker = console.command(f"ext4load scsi 0:4 {LOADADDR} /backups/{args.backup_id}/COMPLETE", timeout=60)
        require(r"\d+\s+bytes read", marker, "backup COMPLETE marker missing")
        for index in range(CHUNK_COUNT):
            restore_chunk(console, args.backup_id, index)
        console.command("scsi reset", timeout=30)
        require(EXPECTED_MODEL, console.command("scsi info", timeout=20), "SSD missing after restore")
        print("[restore] PASS: P3 restored from completed P4 backup", flush=True)
    finally:
        console.close()


if __name__ == "__main__":
    try:
        main()
    except (BootError, OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
