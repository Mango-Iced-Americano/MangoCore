#!/usr/bin/env python3
"""Safely replace only the CPython tools P3 partition through U-Boot."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
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
    sha256,
)


SECTOR_SIZE = 512
P3_START_LBA = 0xA80800
P3_SECTORS = 0x180000
P3_END_LBA = P3_START_LBA + P3_SECTORS
CHUNK_SECTORS = 0x80000
CHUNK_BYTES = CHUNK_SECTORS * SECTOR_SIZE
CHUNK_COUNT = P3_SECTORS // CHUNK_SECTORS
EXPECTED_MODEL = "TS32GMTS400"
EXPECTED_DISK_ID = 0x4D414E47
LOADADDR = "0x9000000098000000"
EXPECTED_PARTITIONS = (
    (1, 0x800, 0x800000, "83"),
    (2, 0x800800, 0x280000, "0c"),
    (3, P3_START_LBA, P3_SECTORS, "83"),
)
BACKUP_CHUNK_BYTES = 64 * 1024 * 1024
BACKUP_CHUNK_COUNT = (P3_SECTORS * SECTOR_SIZE) // BACKUP_CHUNK_BYTES
SAFE_BACKUP_ID = re.compile(r"^[A-Za-z0-9._-]+$")


def uboot_crc(console: UBootConsole, length: int) -> int:
    output = console.command(f"crc32 {LOADADDR} 0x{length:x}", timeout=90)
    return int(require(r"==>\s*([0-9a-f]{8})", output, "U-Boot CRC missing").group(1), 16)


def expected_mbr_crc(disk_image: Path) -> int:
    with disk_image.open("rb") as source:
        sector = source.read(SECTOR_SIZE)
    if len(sector) != SECTOR_SIZE or sector[510:512] != b"\x55\xaa":
        raise BootError(f"invalid source MBR: {disk_image}")
    disk_id = int.from_bytes(sector[440:444], "little")
    if disk_id != EXPECTED_DISK_ID:
        raise BootError(f"unexpected source disk id: {disk_id:#x}")
    for number, start, sectors, part_type in EXPECTED_PARTITIONS:
        entry = sector[446 + (number - 1) * 16 : 446 + number * 16]
        actual_type = entry[4]
        actual_start = int.from_bytes(entry[8:12], "little")
        actual_sectors = int.from_bytes(entry[12:16], "little")
        if (actual_start, actual_sectors, actual_type) != (
            start,
            sectors,
            int(part_type, 16),
        ):
            raise BootError(f"unsafe source partition {number} layout")
    return zlib.crc32(sector)


def validate_manifest(image: Path, manifest_path: Path) -> None:
    try:
        manifest = json.loads(manifest_path.read_text(encoding="ascii"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise BootError(f"cannot read P3 manifest: {manifest_path}") from error

    expected = {
        "role": "mango-tools-p3",
        "image_bytes": P3_SECTORS * SECTOR_SIZE,
        "target_start_lba": P3_START_LBA,
        "target_sectors": P3_SECTORS,
        "target_end_lba_exclusive": P3_END_LBA,
        "chunk_mib": CHUNK_BYTES // (1024 * 1024),
    }
    for key, value in expected.items():
        if manifest.get(key) != value:
            raise BootError(
                f"unsafe P3 manifest field {key}: {manifest.get(key)!r} != {value!r}"
            )
    if image.stat().st_size != expected["image_bytes"]:
        raise BootError(f"unsafe P3 image size: {image.stat().st_size}")
    actual_sha = sha256(image)
    if manifest.get("sha256") != actual_sha:
        raise BootError(
            f"P3 image SHA-256 mismatch: {actual_sha} != {manifest.get('sha256')}"
        )
    print(
        f"[p3] manifest verified: bytes={image.stat().st_size} sha256={actual_sha}",
        flush=True,
    )


def validate_partition_table(output: str) -> None:
    for number, start, sectors, part_type in EXPECTED_PARTITIONS:
        require(
            rf"^\s*{number}\s+{start}\s+{sectors}\s+"
            rf"{EXPECTED_DISK_ID:08x}-{number:02d}\s+{part_type}\s*$",
            output,
            f"unsafe or missing partition {number}",
        )


def make_chunk(image: Path, index: int, destination: Path) -> int:
    remaining = CHUNK_BYTES
    checksum = 0
    with image.open("rb") as source, destination.open("wb") as output:
        source.seek(index * CHUNK_BYTES)
        while remaining:
            data = source.read(min(8 * 1024 * 1024, remaining))
            if not data:
                raise BootError(f"short P3 image while creating chunk {index}")
            output.write(data)
            checksum = zlib.crc32(data, checksum)
            remaining -= len(data)
    if destination.stat().st_size != CHUNK_BYTES:
        raise BootError(f"unsafe chunk size for {destination}")
    return checksum


def transfer(console: UBootConsole, chunk: Path, expected_crc: int, timeout: float) -> None:
    output = console.command(f"tftpboot {LOADADDR} {chunk.name}", timeout=timeout)
    transferred = int(
        require(r"Bytes transferred\s*=\s*(\d+)", output, "TFTP failed").group(1)
    )
    if transferred != CHUNK_BYTES:
        raise BootError(f"short TFTP transfer: {transferred} != {CHUNK_BYTES}")
    actual_crc = uboot_crc(console, CHUNK_BYTES)
    if actual_crc != expected_crc:
        raise BootError(f"TFTP CRC mismatch: {actual_crc:08x} != {expected_crc:08x}")
    print(
        f"[p3] loaded {chunk.name}: bytes={transferred} crc32={actual_crc:08x}",
        flush=True,
    )


def write_and_verify(console: UBootConsole, index: int, expected_crc: int) -> None:
    start_lba = P3_START_LBA + index * CHUNK_SECTORS
    end_lba = start_lba + CHUNK_SECTORS
    if start_lba < P3_START_LBA or end_lba > P3_END_LBA:
        raise BootError(f"refusing out-of-P3 write: {start_lba:#x}..{end_lba:#x}")

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
        f"[p3] verified {start_lba:#x}..{end_lba:#x}: crc32={actual_crc:08x}",
        flush=True,
    )


def verify_installed_file(console: UBootConsole, source: Path) -> None:
    output = console.command(
        f"ext4load scsi 0:3 {LOADADDR} /tests/cpython/{source.name}", timeout=60
    )
    loaded = int(require(r"(\d+)\s+bytes read", output, "P3 verify file not loaded").group(1))
    if loaded != source.stat().st_size:
        raise BootError(f"P3 verify file size mismatch: {loaded} != {source.stat().st_size}")
    actual_crc = uboot_crc(console, loaded)
    expected_crc = crc32(source)
    if actual_crc != expected_crc:
        raise BootError(
            f"P3 verify file CRC mismatch: {actual_crc:08x} != {expected_crc:08x}"
        )
    print(
        f"[p3] installed {source.name} verified: bytes={loaded} crc32={actual_crc:08x}",
        flush=True,
    )


def verify_persist_backup(console: UBootConsole, backup_id: str) -> None:
    """Prove that a completed P3 backup is readable from P4 before any write."""
    prefix = f"/backups/{backup_id}"
    marker = console.command(
        f"ext4load scsi 0:4 {LOADADDR} {prefix}/COMPLETE", timeout=30
    )
    marker_bytes = int(
        require(r"(\d+)\s+bytes read", marker, "P3 backup COMPLETE marker missing").group(1)
    )
    if marker_bytes <= 0 or marker_bytes > 4096:
        raise BootError(f"unsafe P3 backup COMPLETE marker size: {marker_bytes}")

    for index in range(BACKUP_CHUNK_COUNT):
        path = f"{prefix}/p3-{index:02d}.bin"
        output = console.command(
            f"ext4load scsi 0:4 {LOADADDR} {path}", timeout=180
        )
        loaded = int(
            require(r"(\d+)\s+bytes read", output, f"P3 backup chunk missing: {path}").group(1)
        )
        if loaded != BACKUP_CHUNK_BYTES:
            raise BootError(
                f"P3 backup chunk size mismatch for {path}: "
                f"{loaded} != {BACKUP_CHUNK_BYTES}"
            )
        checksum = uboot_crc(console, loaded)
        print(
            f"[p3] backup readable: file={path} bytes={loaded} "
            f"crc32={checksum:08x}",
            flush=True,
        )
    print(
        f"[p3] backup gate PASS: id={backup_id} bytes={P3_SECTORS * SECTOR_SIZE}",
        flush=True,
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interface", default="en8")
    parser.add_argument("--serial")
    parser.add_argument("--tftp-root", type=Path, default=DEFAULT_TFTP_ROOT)
    parser.add_argument(
        "--image", type=Path, default=Path("mango-2k1000la-cpython-tools-p3.img")
    )
    parser.add_argument("--manifest", type=Path)
    parser.add_argument(
        "--verify-file",
        type=Path,
        default=Path("user/tools/cpython/L7_filesystem.py"),
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
    parser.add_argument("--log", type=Path, default=Path("/private/tmp/mango-p3-update.log"))
    parser.add_argument("--no-host-config", dest="configure_host", action="store_false")
    parser.add_argument("--confirm-p3-start", required=True)
    parser.add_argument(
        "--backup-id",
        required=True,
        help="completed /persist/backups/<id> created by backup_2k1000_p3.py",
    )
    parser.set_defaults(configure_host=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    try:
        confirmed_start = int(args.confirm_p3_start, 0)
    except ValueError as error:
        raise BootError("--confirm-p3-start must be an integer") from error
    if confirmed_start != P3_START_LBA:
        raise BootError(
            f"confirmation mismatch: {confirmed_start:#x} != expected {P3_START_LBA:#x}"
        )
    if not SAFE_BACKUP_ID.fullmatch(args.backup_id):
        raise BootError("--backup-id contains unsafe characters")

    image = args.image.expanduser().resolve()
    manifest = (
        args.manifest.expanduser().resolve()
        if args.manifest
        else Path(f"{image}.json")
    )
    verify_file = args.verify_file.expanduser().resolve()
    disk_image = args.disk_image.expanduser().resolve()
    for path in (image, manifest, verify_file, disk_image):
        if not path.is_file():
            raise BootError(f"required file not found: {path}")
    validate_manifest(image, manifest)
    source_mbr_crc = expected_mbr_crc(disk_image)

    try:
        import serial
    except ImportError as error:
        raise BootError("pyserial is required") from error

    ensure_interface(args.interface, args.host_ip, args.netmask, args.configure_host)
    ensure_tftp_service(args.configure_host)
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
        info = console.command("scsi info", timeout=20)
        require(EXPECTED_MODEL, info, "unexpected SSD identity")
        validate_partition_table(console.command("scsi part 0", timeout=20))

        console.command(f"scsi read {LOADADDR} 0x0 0x1", timeout=30)
        actual_mbr_crc = uboot_crc(console, SECTOR_SIZE)
        if actual_mbr_crc != source_mbr_crc:
            raise BootError(
                f"MBR CRC mismatch: board={actual_mbr_crc:08x} "
                f"source={source_mbr_crc:08x}"
            )
        print(
            f"[p3] SSD/MBR verified: model={EXPECTED_MODEL} crc32={actual_mbr_crc:08x}",
            flush=True,
        )
        verify_persist_backup(console, args.backup_id)

        with tempfile.TemporaryDirectory(prefix="mango-p3-chunks-") as tmp_name:
            for index in range(CHUNK_COUNT):
                chunk = Path(tmp_name) / f"mango-cpython-p3.part-{index:02d}"
                chunk_crc = make_chunk(image, index, chunk)
                tftp_chunk = prepare_tftp_image(chunk, args.tftp_root)
                transfer(console, tftp_chunk, chunk_crc, args.tftp_timeout)
                write_and_verify(console, index, chunk_crc)

        scsi = console.command("scsi reset", timeout=30)
        require(EXPECTED_MODEL, scsi, "SSD missing after P3 update")
        validate_partition_table(console.command("scsi part 0", timeout=20))
        listing = console.command("ext4ls scsi 0:3 /tests/cpython", timeout=60)
        require(r"L7_filesystem\.py", listing, "updated CPython tests missing from P3")
        verify_installed_file(console, verify_file)
        print("[p3] PASS: P3 replaced and latest CPython L7 script verified", flush=True)
    finally:
        console.close()


if __name__ == "__main__":
    try:
        main()
    except (BootError, OSError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
