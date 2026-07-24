#!/usr/bin/env python3
"""Build a sparse four-partition QEMU fixture matching the 2K1000LA SSD."""

from __future__ import annotations

import argparse
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile

from make_2k1000_p4_ext4 import (
    MIB,
    P4_BYTES,
    P4_END_LBA,
    P4_SECTORS,
    P4_START_LBA,
    SECTOR_SIZE,
    verify_superblock,
)


DISK_ID = 0x4D414E47
P1_START = 0x800
P1_SECTORS = 0x800000
P2_START = 0x800800
P2_SECTORS = 0x280000
P3_START = 0xA80800
P3_SECTORS = 0x180000
SMALL_EXT4_MIB = 64


def fail(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def require_command(name: str) -> None:
    if shutil.which(name) is None:
        fail(f"required command not found: {name}")


def run(command: list[str]) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, check=True)


def put_partition(mbr: bytearray, index: int, part_type: int, start: int, sectors: int) -> None:
    offset = 446 + index * 16
    mbr[offset] = 0
    mbr[offset + 1 : offset + 4] = b"\xfe\xff\xff"
    mbr[offset + 4] = part_type
    mbr[offset + 5 : offset + 8] = b"\xfe\xff\xff"
    struct.pack_into("<I", mbr, offset + 8, start)
    struct.pack_into("<I", mbr, offset + 12, sectors)


def build_small_ext4(path: Path, label: str, marker: str) -> None:
    staging = path.parent / f"{label.lower()}-root"
    staging.mkdir()
    (staging / marker).write_text(f"{label} QEMU fixture\n", encoding="ascii")
    with path.open("wb") as image:
        image.truncate(SMALL_EXT4_MIB * MIB)
    run(
        [
            "mke2fs",
            "-q",
            "-t",
            "ext4",
            "-F",
            "-b",
            "4096",
            "-m",
            "0",
            "-L",
            label,
            "-O",
            "^has_journal",
            "-d",
            str(staging),
            str(path),
        ]
    )
    run(["e2fsck", "-f", "-n", str(path)])


def sparse_copy(source: Path, destination: Path, start_lba: int) -> None:
    offset_mib = start_lba * SECTOR_SIZE // MIB
    run(
        [
            "dd",
            f"if={source}",
            f"of={destination}",
            "bs=1M",
            f"seek={offset_mib}",
            "conv=notrunc,sparse",
            "status=none",
        ]
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--p4-image", type=Path, default=Path("mango-2k1000la-state-p4.img"))
    parser.add_argument(
        "--output", type=Path, default=Path("mango-2k1000la-p4-qemu.img")
    )
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    for command in ("mke2fs", "e2fsck", "mkfs.vfat", "dd"):
        require_command(command)
    p4_image = args.p4_image.expanduser().resolve()
    output = args.output.expanduser().resolve()
    if not p4_image.is_file() or p4_image.stat().st_size != P4_BYTES:
        fail(f"P4 image is missing or has the wrong size: {p4_image}")
    verify_superblock(p4_image)
    if output.exists() and not args.force:
        fail(f"output already exists (use --force): {output}")

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="mango-p4-qemu-") as tmp_name:
        temporary = Path(tmp_name)
        p1 = temporary / "p1.ext4"
        p3 = temporary / "p3.ext4"
        build_small_ext4(p1, "MANGO_QEMU_P1", "P1.txt")
        build_small_ext4(p3, "MANGO_QEMU_P3", "P3.txt")

        with output.open("wb") as disk:
            disk.truncate(P4_END_LBA * SECTOR_SIZE)
            mbr = bytearray(SECTOR_SIZE)
            struct.pack_into("<I", mbr, 440, DISK_ID)
            put_partition(mbr, 0, 0x83, P1_START, P1_SECTORS)
            put_partition(mbr, 1, 0x0C, P2_START, P2_SECTORS)
            put_partition(mbr, 2, 0x83, P3_START, P3_SECTORS)
            put_partition(mbr, 3, 0x83, P4_START_LBA, P4_SECTORS)
            mbr[510:512] = b"\x55\xaa"
            disk.seek(0)
            disk.write(mbr)

        sparse_copy(p1, output, P1_START)
        sparse_copy(p3, output, P3_START)
        sparse_copy(p4_image, output, P4_START_LBA)
        p2_blocks = P2_SECTORS * SECTOR_SIZE // 1024
        run(
            [
                "mkfs.vfat",
                "-F",
                "32",
                "-n",
                "MANGO_FAT",
                "-i",
                "4D414E47",
                "--offset",
                str(P2_START),
                str(output),
                str(p2_blocks),
            ]
        )

    with output.open("rb") as disk:
        mbr = disk.read(SECTOR_SIZE)
    if mbr[510:512] != b"\x55\xaa" or output.stat().st_size != P4_END_LBA * SECTOR_SIZE:
        fail("QEMU fixture verification failed")
    print(f"[p4-qemu] output={output}")
    print(f"[p4-qemu] logical_bytes={output.stat().st_size}")
    print(f"[p4-qemu] P4={P4_START_LBA:#x}..{P4_END_LBA:#x}")


if __name__ == "__main__":
    main()
