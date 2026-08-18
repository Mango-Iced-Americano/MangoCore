#!/usr/bin/env python3
"""Build a legacy-MBR test disk for the Loongson 2K1000LA board.

Layout:
  partition 1: complete LA64 oscomp test image (ext4)
  partition 2: unmounted FAT32 scratch filesystem (/dev/vda2 compatibility)
  partition 3: MangoCore diagnostic tools (ext4)

GPT and extended partitions are intentionally not used.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
from typing import Optional


SECTOR_SIZE = 512
MIB = 1024 * 1024
ALIGN_SECTORS = MIB // SECTOR_SIZE
MBR_DISK_ID = 0x4D414E47
COPY_CHUNK = 8 * MIB

TOOLS_UUID = "4d414e47-544f-4f4c-5300-000000000002"
TOOLS_TESTS = ("inet_test", "fs_test", "unix_test")
BUSYBOX_APPLETS = (
    "awk",
    "basename",
    "blkid",
    "blockdev",
    "cat",
    "chmod",
    "cp",
    "dd",
    "df",
    "dirname",
    "false",
    "grep",
    "ln",
    "ls",
    "mkdir",
    "mkfs.vfat",
    "mount",
    "mv",
    "printf",
    "rm",
    "sed",
    "sha256sum",
    "sleep",
    "stat",
    "sync",
    "test",
    "tr",
    "true",
    "umount",
    "uname",
    "wc",
)
MUSL_LINKS = (
    "ld-musl-loongarch-lp64d.so.1",
    "libc.musl-loongarch64.so.1",
)


def fail(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def align_up(value: int, alignment: int) -> int:
    return (value + alignment - 1) // alignment * alignment


def run(command: list[str]) -> None:
    print("+", " ".join(command))
    subprocess.run(command, check=True)


def require_command(name: str) -> None:
    if shutil.which(name) is None:
        fail(f"required command not found: {name}")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(COPY_CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def replace_symlink(path: Path, target: str) -> None:
    if path.is_symlink() or path.exists():
        path.unlink()
    path.symlink_to(target)


def put_partition_entry(
    mbr: bytearray,
    index: int,
    part_type: int,
    start_lba: int,
    sectors: int,
) -> None:
    if not 0 <= index < 4:
        fail(f"invalid MBR partition index: {index}")
    if start_lba > 0xFFFFFFFF or sectors > 0xFFFFFFFF:
        fail("partition does not fit in legacy MBR fields")

    offset = 446 + index * 16
    mbr[offset] = 0x00
    mbr[offset + 1 : offset + 4] = b"\xfe\xff\xff"
    mbr[offset + 4] = part_type
    mbr[offset + 5 : offset + 8] = b"\xfe\xff\xff"
    struct.pack_into("<I", mbr, offset + 8, start_lba)
    struct.pack_into("<I", mbr, offset + 12, sectors)


def copy_into(source: Path, destination, offset: int) -> None:
    destination.seek(offset)
    with source.open("rb") as src:
        while True:
            data = src.read(COPY_CHUNK)
            if not data:
                break
            destination.write(data)


def build_tools_payload(
    tools_root: Path,
    user_bin_dir: Path,
    size_mib: int,
    temporary_dir: Path,
    kernel_image: Optional[Path] = None,
) -> Path:
    staging = temporary_dir / "tools-root"
    shutil.copytree(tools_root, staging, symlinks=True)

    for relative in (
        "bin",
        "sbin",
        "lib",
        "usr/bin",
        "usr/sbin",
        "etc",
        "tests",
        "dev/shm",
        "proc",
        "root",
        "run",
        "sys",
        "tmp",
        "var/log",
        "var/tmp",
    ):
        (staging / relative).mkdir(parents=True, exist_ok=True)

    runtime_dir = staging / "tests" / "cpython"
    source_runtime_dir = tools_root / "tests" / "cpython"
    if source_runtime_dir.is_dir():
        cpython_common = tools_root.parent / "cpython"
        if not cpython_common.is_dir():
            fail(f"CPython runtime is present but common scripts are missing: {cpython_common}")
        shutil.copytree(cpython_common, runtime_dir, dirs_exist_ok=True, symlinks=True)
        if not (runtime_dir / "usr" / "bin" / "python3").is_file():
            fail(f"incomplete CPython runtime: {source_runtime_dir}")
        launcher = staging / "usr" / "bin" / "python3"
        if not launcher.is_file():
            fail("tools root must provide usr/bin/python3 launcher")
        launcher.chmod(0o755)
        replace_symlink(
            staging / "usr" / "bin" / "python",
            "python3",
        )

    for command in ("curl", "apk", "persist-shell", "mango-apk-bootstrap"):
        launcher = staging / "usr" / "bin" / command
        if not launcher.is_file():
            fail(f"tools root must provide usr/bin/{command} launcher")
        launcher.chmod(0o755)

    for test_name in TOOLS_TESTS:
        source = user_bin_dir / test_name
        if not source.is_file():
            fail(f"required LA64 user binary not found: {source}")
        shutil.copy2(source, staging / "tests" / test_name)

    busybox = staging / "bin" / "busybox"
    bash = staging / "bin" / "bash"
    if not busybox.is_file() or not bash.is_file():
        fail("tools root must contain bin/busybox and bin/bash")
    init_script = staging / "etc" / "init.d" / "rcS"
    inittab = staging / "etc" / "inittab"
    if not init_script.is_file() or not inittab.is_file():
        fail("tools root must contain etc/init.d/rcS and etc/inittab")
    init_script.chmod(0o755)
    replace_symlink(staging / "sbin" / "init", "../bin/busybox")
    for applet in BUSYBOX_APPLETS:
        replace_symlink(staging / "bin" / applet, "busybox")
    replace_symlink(staging / "bin" / "sh", "bash")

    libc_so = staging / "lib" / "libc.so"
    if libc_so.is_file():
        for link_name in MUSL_LINKS:
            replace_symlink(staging / "lib" / link_name, "libc.so")

    if kernel_image is not None:
        if not kernel_image.is_file():
            fail(f"local boot kernel not found: {kernel_image}")
        with kernel_image.open("rb") as image:
            if image.read(4) != b"\x27\x05\x19\x56":
                fail(f"local boot kernel is not a legacy uImage: {kernel_image}")
        boot_dir = staging / "boot"
        boot_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(kernel_image, boot_dir / "kernel-A.ui")
        (boot_dir / "kernel-A.sha256").write_text(
            f"{sha256_file(kernel_image)}  kernel-A.ui\n",
            encoding="ascii",
        )

    (staging / "MANGO_TOOLS.txt").write_text(
        "MangoCore 2K1000LA persistent mainline root\n"
        "source=user/tools/loongarch64\n"
        "init=/sbin/init -> /bin/busybox\n"
        f"tests={','.join(TOOLS_TESTS)}\n",
        encoding="ascii",
    )

    payload = temporary_dir / "tools.ext4"
    with payload.open("wb") as image:
        image.truncate(size_mib * MIB)
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
            "-O",
            "^has_journal",
            "-L",
            "MANGO_TOOLS",
            "-U",
            TOOLS_UUID,
            "-d",
            str(staging),
            str(payload),
        ]
    )
    run(["e2fsck", "-f", "-n", str(payload)])
    return payload


def verify_embedded_payload(source: Path, disk: Path, offset: int) -> str:
    source_digest = hashlib.sha256()
    embedded_digest = hashlib.sha256()
    remaining = source.stat().st_size
    with source.open("rb") as src, disk.open("rb") as image:
        image.seek(offset)
        while remaining:
            amount = min(COPY_CHUNK, remaining)
            src_data = src.read(amount)
            disk_data = image.read(amount)
            if src_data != disk_data:
                fail(f"embedded payload differs from {source} at byte {source.tell() - amount}")
            source_digest.update(src_data)
            embedded_digest.update(disk_data)
            remaining -= amount
    if source_digest.digest() != embedded_digest.digest():
        fail(f"SHA-256 mismatch after embedding {source}")
    return source_digest.hexdigest()


def verify_mbr(
    output: Path,
    expected: list[tuple[int, int, int]],
    total_bytes: int,
) -> None:
    if output.stat().st_size != total_bytes:
        fail("output image size does not match the partition layout")
    with output.open("rb") as image:
        mbr = image.read(SECTOR_SIZE)
    if mbr[510:512] != b"\x55\xaa":
        fail("missing MBR 0x55aa signature")
    if struct.unpack_from("<I", mbr, 440)[0] != MBR_DISK_ID:
        fail("unexpected MBR disk identifier")
    for index, (part_type, start_lba, sectors) in enumerate(expected):
        offset = 446 + index * 16
        actual = (
            mbr[offset + 4],
            struct.unpack_from("<I", mbr, offset + 8)[0],
            struct.unpack_from("<I", mbr, offset + 12)[0],
        )
        if actual != (part_type, start_lba, sectors):
            fail(f"partition {index + 1} does not match the requested layout")


def verify_fat32(output: Path, start_lba: int) -> None:
    with output.open("rb") as image:
        image.seek(start_lba * SECTOR_SIZE)
        boot_sector = image.read(SECTOR_SIZE)
    if boot_sector[510:512] != b"\x55\xaa" or b"FAT32" not in boot_sector:
        fail("FAT32 scratch partition boot sector is invalid")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Build the complete 2K1000LA MBR test disk image"
    )
    parser.add_argument("--official-img", required=True, type=Path)
    parser.add_argument("--tools-root", required=True, type=Path)
    parser.add_argument("--user-bin-dir", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--tools-mib", type=int, default=768)
    parser.add_argument("--scratch-mib", type=int, default=1280)
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    for command in ("mke2fs", "e2fsck", "mkfs.vfat"):
        require_command(command)
    if not args.official_img.is_file():
        fail(f"official image not found: {args.official_img}")
    if not args.tools_root.is_dir():
        fail(f"tools root not found: {args.tools_root}")
    if not args.user_bin_dir.is_dir():
        fail(f"user binary directory not found: {args.user_bin_dir}")
    if args.output.exists() and not args.force:
        fail(f"output already exists (use --force): {args.output}")
    if args.tools_mib <= 0 or args.scratch_mib < 64:
        fail("tools size must be positive and FAT32 scratch must be at least 64 MiB")
    if args.official_img.resolve() == args.output.resolve():
        fail("input and output image paths must differ")

    official_size = args.official_img.stat().st_size
    if official_size == 0 or official_size % SECTOR_SIZE != 0:
        fail("official image size must be a non-zero multiple of 512 bytes")

    p1_start = ALIGN_SECTORS
    p1_sectors = align_up(official_size, MIB) // SECTOR_SIZE
    p2_start = align_up(p1_start + p1_sectors, ALIGN_SECTORS)
    p2_sectors = args.scratch_mib * MIB // SECTOR_SIZE
    p3_start = align_up(p2_start + p2_sectors, ALIGN_SECTORS)
    p3_sectors = args.tools_mib * MIB // SECTOR_SIZE
    total_sectors = p3_start + p3_sectors
    total_bytes = total_sectors * SECTOR_SIZE

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="mango-2k1000-") as tmp_name:
        tools_payload = build_tools_payload(
            args.tools_root,
            args.user_bin_dir,
            args.tools_mib,
            Path(tmp_name),
        )

        with args.output.open("wb") as output:
            output.truncate(total_bytes)
            mbr = bytearray(SECTOR_SIZE)
            struct.pack_into("<I", mbr, 440, MBR_DISK_ID)
            put_partition_entry(mbr, 0, 0x83, p1_start, p1_sectors)
            put_partition_entry(mbr, 1, 0x0C, p2_start, p2_sectors)
            put_partition_entry(mbr, 2, 0x83, p3_start, p3_sectors)
            mbr[510:512] = b"\x55\xaa"
            output.seek(0)
            output.write(mbr)
            copy_into(args.official_img, output, p1_start * SECTOR_SIZE)
            copy_into(tools_payload, output, p3_start * SECTOR_SIZE)

        scratch_blocks = p2_sectors * SECTOR_SIZE // 1024
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
                str(p2_start),
                str(args.output),
                str(scratch_blocks),
            ]
        )

        expected = [
            (0x83, p1_start, p1_sectors),
            (0x0C, p2_start, p2_sectors),
            (0x83, p3_start, p3_sectors),
        ]
        verify_mbr(args.output, expected, total_bytes)
        official_sha256 = verify_embedded_payload(
            args.official_img, args.output, p1_start * SECTOR_SIZE
        )
        tools_sha256 = verify_embedded_payload(
            tools_payload, args.output, p3_start * SECTOR_SIZE
        )
        verify_fat32(args.output, p2_start)

    manifest = {
        "format": "legacy-mbr",
        "disk_id": f"0x{MBR_DISK_ID:08x}",
        "image_bytes": total_bytes,
        "partitions": [
            {
                "number": 1,
                "role": "official-tests",
                "type": "0x83",
                "start_lba": p1_start,
                "sectors": p1_sectors,
                "payload_sha256": official_sha256,
            },
            {
                "number": 2,
                "role": "fat32-scratch",
                "type": "0x0c",
                "start_lba": p2_start,
                "sectors": p2_sectors,
            },
            {
                "number": 3,
                "role": "mango-tools",
                "type": "0x83",
                "start_lba": p3_start,
                "sectors": p3_sectors,
                "payload_sha256": tools_sha256,
            },
        ],
    }
    manifest_path = Path(f"{args.output}.layout.json")
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="ascii")

    print(f"[mango-disk] output: {args.output}")
    print(f"[mango-disk] size: {total_bytes} bytes ({total_bytes // MIB} MiB)")
    for partition in manifest["partitions"]:
        print(
            "[mango-disk] p{number}: role={role} type={type} "
            "start={start_lba} sectors={sectors}".format(**partition)
        )
    print(f"[mango-disk] manifest: {manifest_path}")


if __name__ == "__main__":
    main()
