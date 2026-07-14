#!/usr/bin/env python3
"""Build the fixed 4 GiB ext4 P4 payload for 2K1000LA persistent state."""

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
import zlib


SECTOR_SIZE = 512
MIB = 1024 * 1024
P4_START_LBA = 0xC00800
P4_MIB = 4096
P4_BYTES = P4_MIB * MIB
P4_SECTORS = P4_BYTES // SECTOR_SIZE
P4_END_LBA = P4_START_LBA + P4_SECTORS
P4_LABEL = "MANGO_STATE"
P4_UUID = "4d414e47-5354-4154-4500-000000000004"
P4_UUID_BYTES = bytes.fromhex(P4_UUID.replace("-", ""))
CHUNK_MIB = 256
MARKER_PATH = "/MANGO_STATE.txt"
MARKER = (
    b"MangoCore 2K1000LA persistent state partition\n"
    b"schema=1\n"
    b"mount=/persist\n"
    b"apk_root=/persist/apk-root\n"
)


def fail(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def require_command(name: str) -> None:
    if shutil.which(name) is None:
        fail(f"required command not found: {name}")


def run(command: list[str]) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, check=True)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as image:
        while data := image.read(8 * MIB):
            digest.update(data)
    return digest.hexdigest()


def verify_superblock(image: Path) -> None:
    with image.open("rb") as source:
        source.seek(1024)
        superblock = source.read(1024)
    if len(superblock) != 1024:
        fail("short ext4 superblock")
    if struct.unpack_from("<H", superblock, 0x38)[0] != 0xEF53:
        fail("ext4 magic is missing")
    log_block_size = struct.unpack_from("<I", superblock, 0x18)[0]
    if 1024 << log_block_size != 4096:
        fail("P4 ext4 block size is not 4096 bytes")
    compat = struct.unpack_from("<I", superblock, 0x5C)[0]
    incompat = struct.unpack_from("<I", superblock, 0x60)[0]
    if compat & 0x4:
        fail("P4 unexpectedly contains an ext4 journal")
    if incompat & 0x4:
        fail("P4 ext4 filesystem requires journal recovery")
    if superblock[0x68:0x78] != P4_UUID_BYTES:
        fail("P4 ext4 UUID mismatch")
    actual_label = superblock[0x78:0x88].rstrip(b"\0").decode("ascii")
    if actual_label != P4_LABEL:
        fail(f"P4 ext4 label mismatch: {actual_label!r}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output", type=Path, default=Path("mango-2k1000la-state-p4.img")
    )
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    for command in ("mke2fs", "e2fsck"):
        require_command(command)
    output = args.output.expanduser().resolve()
    if output.exists() and not args.force:
        fail(f"output already exists (use --force): {output}")

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="mango-p4-state-") as tmp_name:
        staging = Path(tmp_name) / "root"
        for relative in (
            "apk-root/lib/apk/db",
            "apk-root/etc/apk",
            "apk-root/var/cache/apk",
            "apk-state",
        ):
            (staging / relative).mkdir(parents=True, exist_ok=True)
        (staging / MARKER_PATH.lstrip("/")).write_bytes(MARKER)
        (staging / "apk-root/etc/apk/world").write_bytes(b"")

        with output.open("wb") as image:
            image.truncate(P4_BYTES)
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
                P4_LABEL,
                "-U",
                P4_UUID,
                "-O",
                "^has_journal",
                "-E",
                "lazy_itable_init=0",
                "-d",
                str(staging),
                str(output),
            ]
        )
    run(["e2fsck", "-f", "-n", str(output)])

    if output.stat().st_size != P4_BYTES:
        fail(f"unsafe P4 image size: {output.stat().st_size} != {P4_BYTES}")
    verify_superblock(output)
    digest = sha256_file(output)
    manifest = {
        "schema": 1,
        "role": "mango-state-p4",
        "filesystem": "ext4",
        "filesystem_has_journal": False,
        "filesystem_label": P4_LABEL,
        "filesystem_uuid": P4_UUID,
        "image_bytes": P4_BYTES,
        "sha256": digest,
        "target_start_lba": P4_START_LBA,
        "target_sectors": P4_SECTORS,
        "target_end_lba_exclusive": P4_END_LBA,
        "chunk_mib": CHUNK_MIB,
        "verify_file": MARKER_PATH,
        "verify_file_bytes": len(MARKER),
        "verify_file_crc32": f"{zlib.crc32(MARKER):08x}",
    }
    manifest_path = Path(f"{output}.json")
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="ascii")

    print(f"[p4-image] output={output}")
    print(f"[p4-image] bytes={P4_BYTES} sectors={P4_SECTORS}")
    print(f"[p4-image] target={P4_START_LBA:#x}..{P4_END_LBA:#x}")
    print(f"[p4-image] label={P4_LABEL} uuid={P4_UUID} journal=off")
    print(f"[p4-image] sha256={digest}")
    print(f"[p4-image] manifest={manifest_path}")


if __name__ == "__main__":
    main()
