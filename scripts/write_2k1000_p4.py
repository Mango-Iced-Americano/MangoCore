#!/usr/bin/env python3
"""Safely create the fixed ext4 P4 partition through 2K1000LA U-Boot."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import zlib

from boot_2k1000_tftp import (
    BootError,
    DEFAULT_TFTP_ROOT,
    UBootConsole,
    detect_serial,
    ensure_interface,
    ensure_tftp_service,
    prepare_tftp_image,
    release_matching_screen,
    require,
    sha256,
)
from make_2k1000_p4_ext4 import (
    CHUNK_MIB,
    MARKER,
    MARKER_PATH,
    P4_BYTES,
    P4_END_LBA,
    P4_LABEL,
    P4_SECTORS,
    P4_START_LBA,
    P4_UUID,
)


SECTOR_SIZE = 512
CHUNK_BYTES = CHUNK_MIB * 1024 * 1024
CHUNK_SECTORS = CHUNK_BYTES // SECTOR_SIZE
CHUNK_COUNT = P4_BYTES // CHUNK_BYTES
EXPECTED_MODEL = "TS32GMTS400"
EXPECTED_DISK_SECTORS = 62_533_296
EXPECTED_DISK_ID = 0x4D414E47
LOADADDR = "0x9000000098000000"
BASE_PARTITIONS = (
    (1, 0x800, 0x800000, 0x83),
    (2, 0x800800, 0x280000, 0x0C),
    (3, 0xA80800, 0x180000, 0x83),
)
P4_PARTITION = (4, P4_START_LBA, P4_SECTORS, 0x83)


def uboot_crc(console: UBootConsole, length: int) -> int:
    output = console.command(f"crc32 {LOADADDR} 0x{length:x}", timeout=90)
    return int(require(r"==>\s*([0-9a-f]{8})", output, "U-Boot CRC missing").group(1), 16)


def file_crc32(path: Path) -> int:
    checksum = 0
    with path.open("rb") as source:
        while data := source.read(8 * 1024 * 1024):
            checksum = zlib.crc32(data, checksum)
    return checksum


def partition_entry(mbr: bytes, number: int) -> tuple[int, int, int, int]:
    offset = 446 + (number - 1) * 16
    entry = mbr[offset : offset + 16]
    return (
        entry[0],
        entry[4],
        int.from_bytes(entry[8:12], "little"),
        int.from_bytes(entry[12:16], "little"),
    )


def prepare_mbrs(source: Path) -> tuple[bytes, bytes]:
    with source.open("rb") as image:
        old = bytearray(image.read(SECTOR_SIZE))
    if len(old) != SECTOR_SIZE or old[510:512] != b"\x55\xaa":
        raise BootError(f"invalid source MBR: {source}")
    disk_id = int.from_bytes(old[440:444], "little")
    if disk_id != EXPECTED_DISK_ID:
        raise BootError(f"unexpected source disk id: {disk_id:#x}")
    for number, start, sectors, part_type in BASE_PARTITIONS:
        bootable, actual_type, actual_start, actual_sectors = partition_entry(old, number)
        if (bootable, actual_type, actual_start, actual_sectors) != (
            0,
            part_type,
            start,
            sectors,
        ):
            raise BootError(f"unsafe source partition {number} layout")
    if any(old[446 + 3 * 16 : 446 + 4 * 16]):
        raise BootError("source MBR P4 entry is not empty")

    new = bytearray(old)
    offset = 446 + 3 * 16
    new[offset] = 0
    new[offset + 1 : offset + 4] = b"\xfe\xff\xff"
    new[offset + 4] = 0x83
    new[offset + 5 : offset + 8] = b"\xfe\xff\xff"
    new[offset + 8 : offset + 12] = P4_START_LBA.to_bytes(4, "little")
    new[offset + 12 : offset + 16] = P4_SECTORS.to_bytes(4, "little")
    return bytes(old), bytes(new)


def validate_manifest(image: Path, manifest_path: Path) -> dict[str, object]:
    try:
        manifest = json.loads(manifest_path.read_text(encoding="ascii"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise BootError(f"cannot read P4 manifest: {manifest_path}") from error
    expected: dict[str, object] = {
        "schema": 1,
        "role": "mango-state-p4",
        "filesystem": "ext4",
        "filesystem_has_journal": False,
        "filesystem_label": P4_LABEL,
        "filesystem_uuid": P4_UUID,
        "image_bytes": P4_BYTES,
        "target_start_lba": P4_START_LBA,
        "target_sectors": P4_SECTORS,
        "target_end_lba_exclusive": P4_END_LBA,
        "chunk_mib": CHUNK_MIB,
        "verify_file": MARKER_PATH,
        "verify_file_bytes": len(MARKER),
        "verify_file_crc32": f"{zlib.crc32(MARKER):08x}",
    }
    for key, value in expected.items():
        if manifest.get(key) != value:
            raise BootError(f"unsafe P4 manifest field {key}: {manifest.get(key)!r} != {value!r}")
    if image.stat().st_size != P4_BYTES:
        raise BootError(f"unsafe P4 image size: {image.stat().st_size}")
    actual_sha = sha256(image)
    if manifest.get("sha256") != actual_sha:
        raise BootError(f"P4 SHA-256 mismatch: {actual_sha} != {manifest.get('sha256')}")
    print(f"[p4] manifest verified: bytes={P4_BYTES} sha256={actual_sha}", flush=True)
    return manifest


def validate_partition_table(output: str, expect_p4: bool) -> None:
    expected = BASE_PARTITIONS + ((P4_PARTITION,) if expect_p4 else ())
    for number, start, sectors, part_type in expected:
        require(
            rf"^\s*{number}\s+{start}\s+{sectors}\s+"
            rf"{EXPECTED_DISK_ID:08x}-{number:02d}\s+{part_type:02x}\s*$",
            output,
            f"unsafe or missing partition {number}",
        )
    has_p4 = re.search(r"^\s*4\s+", output, re.MULTILINE) is not None
    if has_p4 != expect_p4:
        raise BootError(f"unexpected P4 presence: {has_p4}")


def validate_capacity(output: str) -> None:
    match = require(
        r"Capacity:.*\((\d+)\s+x\s+512\)",
        output,
        "SSD capacity missing",
    )
    sectors = int(match.group(1))
    if sectors != EXPECTED_DISK_SECTORS:
        raise BootError(f"unexpected SSD capacity: {sectors} != {EXPECTED_DISK_SECTORS}")
    if P4_END_LBA > sectors:
        raise BootError(f"P4 end {P4_END_LBA:#x} exceeds SSD capacity {sectors:#x}")


def make_chunk(image: Path, index: int, destination: Path) -> int:
    remaining = CHUNK_BYTES
    checksum = 0
    with image.open("rb") as source, destination.open("wb") as output:
        source.seek(index * CHUNK_BYTES)
        while remaining:
            data = source.read(min(8 * 1024 * 1024, remaining))
            if not data:
                raise BootError(f"short P4 image while creating chunk {index}")
            output.write(data)
            checksum = zlib.crc32(data, checksum)
            remaining -= len(data)
    if destination.stat().st_size != CHUNK_BYTES:
        raise BootError(f"unsafe P4 chunk size: {destination}")
    return checksum


def transfer(console: UBootConsole, image: Path, expected_crc: int, timeout: float) -> None:
    expected_size = image.stat().st_size
    output = console.command(f"tftpboot {LOADADDR} {image.name}", timeout=timeout)
    transferred = int(require(r"Bytes transferred\s*=\s*(\d+)", output, "TFTP failed").group(1))
    if transferred != expected_size:
        raise BootError(f"short TFTP transfer: {transferred} != {expected_size}")
    actual_crc = uboot_crc(console, expected_size)
    if actual_crc != expected_crc:
        raise BootError(f"TFTP CRC mismatch: {actual_crc:08x} != {expected_crc:08x}")
    print(f"[p4] loaded {image.name}: bytes={transferred} crc32={actual_crc:08x}", flush=True)


def publish_tftp_chunk(chunk: Path, tftp_root: Path) -> tuple[Path, bool]:
    """Publish through a user-owned fixed slot when the TFTP root is root-owned."""
    candidates = (tftp_root / chunk.name, tftp_root / "mango-cpython-p3.part-00")
    for destination in candidates:
        if destination.is_file() and os.access(destination, os.W_OK):
            with chunk.open("rb") as source, destination.open("wb") as output:
                shutil.copyfileobj(source, output, 8 * 1024 * 1024)
                output.flush()
                os.fsync(output.fileno())
            if destination.stat().st_size != chunk.stat().st_size:
                raise BootError(f"short pre-existing TFTP slot: {destination}")
            if file_crc32(destination) != file_crc32(chunk):
                raise BootError(f"pre-existing TFTP slot CRC mismatch: {destination}")
            print(f"[host] refreshed existing TFTP slot: {destination}", flush=True)
            return destination, False
    return prepare_tftp_image(chunk, tftp_root), True


def write_and_verify(
    console: UBootConsole,
    start_lba: int,
    sectors: int,
    expected_crc: int,
    *,
    lower: int,
    upper: int,
) -> None:
    end_lba = start_lba + sectors
    if start_lba < lower or end_lba > upper:
        raise BootError(f"refusing out-of-range write: {start_lba:#x}..{end_lba:#x}")
    write = console.command(f"scsi write {LOADADDR} {start_lba:#x} {sectors:#x}", timeout=300)
    require(rf"{sectors}\s+blocks written:\s+OK", write, f"SCSI write failed at {start_lba:#x}")
    read = console.command(f"scsi read {LOADADDR} {start_lba:#x} {sectors:#x}", timeout=300)
    require(rf"{sectors}\s+blocks read:\s+OK", read, f"SCSI readback failed at {start_lba:#x}")
    actual_crc = uboot_crc(console, sectors * SECTOR_SIZE)
    if actual_crc != expected_crc:
        raise BootError(
            f"readback CRC mismatch at {start_lba:#x}: {actual_crc:08x} != {expected_crc:08x}"
        )
    print(f"[p4] verified {start_lba:#x}..{end_lba:#x}: crc32={actual_crc:08x}", flush=True)


def remove_tftp_copy(path: Path, tftp_root: Path) -> None:
    try:
        if path.parent.resolve() == tftp_root.resolve():
            path.unlink(missing_ok=True)
    except OSError:
        pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interface", default="en8")
    parser.add_argument("--serial")
    parser.add_argument("--tftp-root", type=Path, default=DEFAULT_TFTP_ROOT)
    parser.add_argument("--image", type=Path, default=Path("mango-2k1000la-state-p4.img"))
    parser.add_argument("--manifest", type=Path)
    parser.add_argument(
        "--mbr-source",
        type=Path,
        default=DEFAULT_TFTP_ROOT / "mango-2k1000la-full-test-mbr.img",
    )
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--board-ip", default="192.168.9.20")
    parser.add_argument("--netmask", default="255.255.255.0")
    parser.add_argument("--reset-timeout", type=float, default=120)
    parser.add_argument("--tftp-timeout", type=float, default=360)
    parser.add_argument("--log", type=Path, default=Path("/private/tmp/mango-p4-write.log"))
    parser.add_argument("--no-host-config", dest="configure_host", action="store_false")
    parser.add_argument("--preflight-only", action="store_true")
    parser.add_argument("--replace-existing-p4", action="store_true")
    parser.add_argument("--confirm-p4-start", required=True)
    parser.add_argument("--confirm-p4-end", required=True)
    parser.add_argument("--confirm-disk-sectors", required=True)
    parser.set_defaults(configure_host=True)
    return parser.parse_args()


def parse_confirmation(value: str, option: str) -> int:
    try:
        return int(value, 0)
    except ValueError as error:
        raise BootError(f"{option} must be an integer") from error


def main() -> None:
    args = parse_args()
    confirmations = (
        (parse_confirmation(args.confirm_p4_start, "--confirm-p4-start"), P4_START_LBA, "P4 start"),
        (parse_confirmation(args.confirm_p4_end, "--confirm-p4-end"), P4_END_LBA, "P4 end"),
        (
            parse_confirmation(args.confirm_disk_sectors, "--confirm-disk-sectors"),
            EXPECTED_DISK_SECTORS,
            "disk sectors",
        ),
    )
    for actual, expected, label in confirmations:
        if actual != expected:
            raise BootError(f"{label} confirmation mismatch: {actual:#x} != {expected:#x}")

    image = args.image.expanduser().resolve()
    manifest_path = (
        args.manifest.expanduser().resolve() if args.manifest else Path(f"{image}.json")
    )
    mbr_source = args.mbr_source.expanduser().resolve()
    for path in (image, manifest_path, mbr_source):
        if not path.is_file():
            raise BootError(f"required file not found: {path}")
    validate_manifest(image, manifest_path)
    old_mbr, new_mbr = prepare_mbrs(mbr_source)
    old_mbr_crc = zlib.crc32(old_mbr)
    new_mbr_crc = zlib.crc32(new_mbr)

    try:
        import serial
    except ImportError as error:
        raise BootError("pyserial is required") from error

    ensure_interface(args.interface, args.host_ip, args.netmask, args.configure_host)
    ensure_tftp_service(args.configure_host, args.tftp_root)
    serial_path = detect_serial(args.serial)
    release_matching_screen(serial_path, True)
    console = UBootConsole(serial, serial_path, args.log)
    mbr_attempted = False
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
        validate_capacity(info)
        validate_partition_table(
            console.command("scsi part 0", timeout=20), args.replace_existing_p4
        )
        console.command(f"scsi read {LOADADDR} 0x0 0x1", timeout=30)
        board_mbr_crc = uboot_crc(console, SECTOR_SIZE)
        expected_board_mbr_crc = new_mbr_crc if args.replace_existing_p4 else old_mbr_crc
        if board_mbr_crc != expected_board_mbr_crc:
            raise BootError(
                f"MBR CRC mismatch: board={board_mbr_crc:08x} "
                f"expected={expected_board_mbr_crc:08x}"
            )
        print(
            f"[p4] preflight PASS: model={EXPECTED_MODEL} sectors={EXPECTED_DISK_SECTORS} "
            f"mbr_crc32={board_mbr_crc:08x} target={P4_START_LBA:#x}..{P4_END_LBA:#x}",
            flush=True,
        )
        if args.preflight_only:
            return

        with tempfile.TemporaryDirectory(prefix="mango-p4-write-") as tmp_name:
            temporary = Path(tmp_name)
            old_mbr_path = temporary / "mango-p4-old-mbr.bin"
            new_mbr_path = temporary / "mango-p4-new-mbr.bin"
            old_mbr_path.write_bytes(old_mbr)
            new_mbr_path.write_bytes(new_mbr)

            for index in range(CHUNK_COUNT):
                chunk = temporary / f"mango-p4.part-{index:02d}"
                expected_crc = make_chunk(image, index, chunk)
                tftp_chunk, remove_chunk = publish_tftp_chunk(chunk, args.tftp_root)
                try:
                    transfer(console, tftp_chunk, expected_crc, args.tftp_timeout)
                    start_lba = P4_START_LBA + index * CHUNK_SECTORS
                    write_and_verify(
                        console,
                        start_lba,
                        CHUNK_SECTORS,
                        expected_crc,
                        lower=P4_START_LBA,
                        upper=P4_END_LBA,
                    )
                finally:
                    if remove_chunk:
                        remove_tftp_copy(tftp_chunk, args.tftp_root)

            if args.replace_existing_p4:
                scsi = console.command("scsi reset", timeout=30)
                require(EXPECTED_MODEL, scsi, "SSD missing after P4 replacement")
                validate_partition_table(console.command("scsi part 0", timeout=20), True)
                listing = console.command("ext4ls scsi 0:4 /", timeout=60)
                require(r"MANGO_STATE\.txt", listing, "P4 marker missing")
                require(r"apk-root", listing, "P4 apk-root missing")
                loaded = console.command(
                    f"ext4load scsi 0:4 {LOADADDR} {MARKER_PATH}", timeout=60
                )
                marker_bytes = int(
                    require(r"(\d+)\s+bytes read", loaded, "P4 marker not loaded").group(1)
                )
                if marker_bytes != len(MARKER):
                    raise BootError(
                        f"P4 marker size mismatch: {marker_bytes} != {len(MARKER)}"
                    )
                marker_crc = uboot_crc(console, marker_bytes)
                if marker_crc != zlib.crc32(MARKER):
                    raise BootError("P4 replacement marker verification failed")
                print(
                    "[p4] PASS: existing P4 payload replaced; MBR preserved; "
                    f"mbr_crc32={new_mbr_crc:08x}",
                    flush=True,
                )
                return

            tftp_new_mbr = prepare_tftp_image(new_mbr_path, args.tftp_root)
            tftp_old_mbr = prepare_tftp_image(old_mbr_path, args.tftp_root)
            try:
                transfer(console, tftp_new_mbr, new_mbr_crc, args.tftp_timeout)
                mbr_attempted = True
                write_and_verify(
                    console,
                    0,
                    1,
                    new_mbr_crc,
                    lower=0,
                    upper=1,
                )
                scsi = console.command("scsi reset", timeout=30)
                require(EXPECTED_MODEL, scsi, "SSD missing after MBR commit")
                validate_partition_table(console.command("scsi part 0", timeout=20), True)
                listing = console.command("ext4ls scsi 0:4 /", timeout=60)
                require(r"MANGO_STATE\.txt", listing, "P4 marker missing")
                require(r"apk-root", listing, "P4 apk-root missing")
                loaded = console.command(f"ext4load scsi 0:4 {LOADADDR} {MARKER_PATH}", timeout=60)
                marker_bytes = int(require(r"(\d+)\s+bytes read", loaded, "P4 marker not loaded").group(1))
                if marker_bytes != len(MARKER):
                    raise BootError(f"P4 marker size mismatch: {marker_bytes} != {len(MARKER)}")
                marker_crc = uboot_crc(console, marker_bytes)
                expected_marker_crc = zlib.crc32(MARKER)
                if marker_crc != expected_marker_crc:
                    raise BootError(
                        f"P4 marker CRC mismatch: {marker_crc:08x} != {expected_marker_crc:08x}"
                    )
                mbr_attempted = False
                print(
                    f"[p4] PASS: payload verified, MBR committed, ext4 label content readable; "
                    f"mbr_crc32={new_mbr_crc:08x}",
                    flush=True,
                )
            except BaseException:
                if mbr_attempted:
                    print("[p4] MBR commit did not finish; attempting old-MBR rollback", file=sys.stderr)
                    try:
                        transfer(console, tftp_old_mbr, old_mbr_crc, args.tftp_timeout)
                        write_and_verify(
                            console,
                            0,
                            1,
                            old_mbr_crc,
                            lower=0,
                            upper=1,
                        )
                        console.command("scsi reset", timeout=30)
                        validate_partition_table(console.command("scsi part 0", timeout=20), False)
                        print("[p4] old MBR rollback verified", file=sys.stderr)
                    except Exception as rollback_error:
                        print(f"[p4] CRITICAL: old MBR rollback failed: {rollback_error}", file=sys.stderr)
                raise
            finally:
                remove_tftp_copy(tftp_new_mbr, args.tftp_root)
                remove_tftp_copy(tftp_old_mbr, args.tftp_root)
    finally:
        console.close()


if __name__ == "__main__":
    try:
        main()
    except (BootError, OSError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
