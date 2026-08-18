#!/usr/bin/env python3
"""Restore only the prepared FAT32 P2 partition through 2K1000LA U-Boot."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import subprocess
import sys
import zlib

from boot_2k1000_tftp import (
    BootError,
    DEFAULT_TFTP_ROOT,
    UBootConsole,
    crc32,
    detect_serial,
    ensure_interface,
    ensure_tftp_service,
    prepare_tftp_image,
    release_matching_screen,
    require,
)


P2_START_LBA = 0x800800
P2_SECTORS = 0x280000
P2_END_LBA = P2_START_LBA + P2_SECTORS
CHUNK_SECTORS = 0x80000
CHUNK_BYTES = CHUNK_SECTORS * 512
EXPECTED_MODEL = "TS32GMTS400"
LOADADDR = "0x9000000098000000"


def uboot_crc(console: UBootConsole, length: int) -> int:
    output = console.command(f"crc32 {LOADADDR} 0x{length:x}", timeout=90)
    return int(require(r"==>\s*([0-9a-f]{8})", output, "U-Boot CRC missing").group(1), 16)


def transfer(console: UBootConsole, image: Path, timeout: float) -> int:
    expected_size = image.stat().st_size
    expected_crc = crc32(image)
    output = console.command(f"tftpboot {LOADADDR} {image.name}", timeout=timeout)
    transferred = int(
        require(r"Bytes transferred\s*=\s*(\d+)", output, "TFTP failed").group(1)
    )
    if transferred != expected_size:
        raise BootError(f"short TFTP transfer: {transferred} != {expected_size}")
    actual_crc = uboot_crc(console, expected_size)
    if actual_crc != expected_crc:
        raise BootError(f"TFTP CRC mismatch: {actual_crc:08x} != {expected_crc:08x}")
    print(
        f"[restore] loaded {image.name}: bytes={transferred} crc32={actual_crc:08x}",
        flush=True,
    )
    return expected_crc


def write_and_verify(console: UBootConsole, start_lba: int, expected_crc: int) -> None:
    end_lba = start_lba + CHUNK_SECTORS
    if start_lba < P2_START_LBA or end_lba > P2_END_LBA:
        raise BootError(
            f"refusing out-of-P2 write: {start_lba:#x}..{end_lba:#x} "
            f"outside {P2_START_LBA:#x}..{P2_END_LBA:#x}"
        )
    if (start_lba - P2_START_LBA) % CHUNK_SECTORS != 0:
        raise BootError(f"refusing unaligned P2 chunk start: {start_lba:#x}")

    write = console.command(
        f"scsi write {LOADADDR} {start_lba:#x} {CHUNK_SECTORS:#x}", timeout=240
    )
    require(
        rf"{CHUNK_SECTORS}\s+blocks written:\s+OK",
        write,
        f"SCSI write failed at {start_lba:#x}",
    )
    read = console.command(
        f"scsi read {LOADADDR} {start_lba:#x} {CHUNK_SECTORS:#x}", timeout=240
    )
    require(
        rf"{CHUNK_SECTORS}\s+blocks read:\s+OK",
        read,
        f"SCSI readback failed at {start_lba:#x}",
    )
    actual_crc = uboot_crc(console, CHUNK_BYTES)
    if actual_crc != expected_crc:
        raise BootError(
            f"readback CRC mismatch at {start_lba:#x}: "
            f"{actual_crc:08x} != {expected_crc:08x}"
        )
    print(
        f"[restore] verified {start_lba:#x}..{end_lba:#x}: crc32={actual_crc:08x}",
        flush=True,
    )


def expected_mbr_crc(disk_image: Path) -> int:
    with disk_image.open("rb") as source:
        sector = source.read(512)
    if len(sector) != 512 or sector[510:512] != b"\x55\xaa":
        raise BootError(f"invalid source MBR: {disk_image}")
    return zlib.crc32(sector)


def is_all_zero(path: Path) -> bool:
    with path.open("rb") as source:
        while data := source.read(4 * 1024 * 1024):
            if any(data):
                return False
    return True


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interface", default="en8")
    parser.add_argument("--serial")
    parser.add_argument("--tftp-root", type=Path, default=DEFAULT_TFTP_ROOT)
    parser.add_argument(
        "--metadata-chunk", type=Path, default=DEFAULT_TFTP_ROOT / "mango-p2-clean.part-00"
    )
    parser.add_argument(
        "--zero-chunk", type=Path, default=DEFAULT_TFTP_ROOT / "mango-p2-clean.part-01"
    )
    parser.add_argument(
        "--disk-image",
        type=Path,
        default=DEFAULT_TFTP_ROOT / "mango-2k1000la-full-test-mbr.img",
    )
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--board-ip", default="192.168.9.20")
    parser.add_argument("--netmask", default="255.255.255.0")
    parser.add_argument("--reset-timeout", type=float, default=120)
    parser.add_argument("--tftp-timeout", type=float, default=300)
    parser.add_argument("--log", type=Path, default=Path("/private/tmp/mango-p2-restore.log"))
    parser.add_argument("--no-host-config", dest="configure_host", action="store_false")
    parser.add_argument("--confirm-p2-start", required=True)
    parser.set_defaults(configure_host=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    try:
        confirmed_start = int(args.confirm_p2_start, 0)
    except ValueError as error:
        raise BootError("--confirm-p2-start must be an integer") from error
    if confirmed_start != P2_START_LBA:
        raise BootError(
            f"confirmation mismatch: {confirmed_start:#x} != expected {P2_START_LBA:#x}"
        )

    try:
        import serial
    except ImportError as error:
        raise BootError("pyserial is required") from error

    ensure_interface(args.interface, args.host_ip, args.netmask, args.configure_host)
    ensure_tftp_service(args.configure_host, args.tftp_root)
    metadata_chunk = prepare_tftp_image(args.metadata_chunk, args.tftp_root)
    zero_chunk = prepare_tftp_image(args.zero_chunk, args.tftp_root)
    if metadata_chunk.stat().st_size != CHUNK_BYTES:
        raise BootError("metadata chunk is not exactly 256 MiB")
    if zero_chunk.stat().st_size != CHUNK_BYTES:
        raise BootError("zero chunk is not exactly 256 MiB")
    if not is_all_zero(zero_chunk):
        raise BootError("zero chunk contains non-zero data")

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

        ping = console.command(f"ping {args.host_ip}", timeout=20)
        require(rf"host\s+{re.escape(args.host_ip)}\s+is\s+alive", ping, "ping failed")
        scsi = console.command("scsi reset", timeout=30)
        require(EXPECTED_MODEL, scsi, "unexpected or missing SSD")

        console.command(f"scsi read {LOADADDR} 0x0 0x1", timeout=30)
        actual_mbr_crc = uboot_crc(console, 512)
        source_mbr_crc = expected_mbr_crc(args.disk_image)
        if actual_mbr_crc != source_mbr_crc:
            raise BootError(
                f"MBR CRC mismatch: board={actual_mbr_crc:08x} source={source_mbr_crc:08x}"
            )
        print(f"[restore] SSD/MBR verified: model={EXPECTED_MODEL} crc32={actual_mbr_crc:08x}")

        metadata_crc = transfer(console, metadata_chunk, args.tftp_timeout)
        write_and_verify(console, P2_START_LBA, metadata_crc)

        zero_crc = transfer(console, zero_chunk, args.tftp_timeout)
        for start_lba in range(
            P2_START_LBA + CHUNK_SECTORS, P2_END_LBA, CHUNK_SECTORS
        ):
            write_and_verify(console, start_lba, zero_crc)

        fat_info = console.command("fatinfo scsi 0:2", timeout=30)
        require(r"Filesystem:\s+FAT32\s+\"MANGO_FAT", fat_info, "restored FAT32 not detected")
        root = console.command("fatls scsi 0:2 /", timeout=30)
        require(r"0 file\(s\),\s+0 dir\(s\)", root, "restored FAT32 root is not empty")
        print("[restore] PASS: P2 restored and empty FAT32 root verified")
    finally:
        console.close()


if __name__ == "__main__":
    try:
        main()
    except (BootError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
