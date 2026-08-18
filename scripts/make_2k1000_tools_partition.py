#!/usr/bin/env python3
"""Build a replacement P3 tools filesystem for the 2K1000LA test SSD."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tempfile

from make_2k1000_full_test_disk import build_tools_payload, fail, require_command


SECTOR_SIZE = 512
BOARD_P3_START_LBA = 0xA80800
BOARD_P3_MIB = 768
BOARD_P3_BYTES = BOARD_P3_MIB * 1024 * 1024
BOARD_P3_SECTORS = BOARD_P3_BYTES // SECTOR_SIZE


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as image:
        while chunk := image.read(8 * 1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tools-root", required=True, type=Path)
    parser.add_argument("--user-bin-dir", required=True, type=Path)
    parser.add_argument("--kernel-image", type=Path)
    parser.add_argument(
        "--apk-package-dir",
        type=Path,
        help="optional host directory of LoongArch APKs to preseed under /apk",
    )
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    for command in ("mke2fs", "e2fsck"):
        require_command(command)
    if not args.tools_root.is_dir():
        fail(f"tools root not found: {args.tools_root}")
    if not args.user_bin_dir.is_dir():
        fail(f"user binary directory not found: {args.user_bin_dir}")
    if args.kernel_image is not None and not args.kernel_image.is_file():
        fail(f"kernel image not found: {args.kernel_image}")
    if args.output.exists() and not args.force:
        fail(f"output already exists (use --force): {args.output}")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="mango-2k1000-tools-") as tmp_name:
        payload = build_tools_payload(
            args.tools_root,
            args.user_bin_dir,
            BOARD_P3_MIB,
            Path(tmp_name),
            args.kernel_image,
            args.apk_package_dir,
        )
        shutil.copyfile(payload, args.output)

    if args.output.stat().st_size != BOARD_P3_BYTES:
        fail(
            f"unsafe P3 image size: {args.output.stat().st_size} "
            f"(expected exactly {BOARD_P3_BYTES})"
        )
    digest = sha256_file(args.output)
    sectors = args.output.stat().st_size // SECTOR_SIZE
    if sectors != BOARD_P3_SECTORS:
        fail(f"unsafe P3 sector count: {sectors} (expected {BOARD_P3_SECTORS})")
    manifest = {
        "role": "mango-tools-p3",
        "image_bytes": args.output.stat().st_size,
        "sha256": digest,
        "target_start_lba": BOARD_P3_START_LBA,
        "target_sectors": sectors,
        "target_end_lba_exclusive": BOARD_P3_START_LBA + sectors,
        "chunk_mib": 256,
        "local_boot_kernel": "/boot/kernel-A.ui" if args.kernel_image else None,
        "local_boot_kernel_sha256": (
            sha256_file(args.kernel_image) if args.kernel_image else None
        ),
    }
    manifest_path = Path(f"{args.output}.json")
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="ascii")
    print(f"[tools-p3] {args.output}: {BOARD_P3_MIB} MiB ({sectors} sectors)")
    print(f"[tools-p3] sha256={digest}")
    print(f"[tools-p3] target LBA=0x{BOARD_P3_START_LBA:x}")
    print(f"[tools-p3] manifest={manifest_path}")


if __name__ == "__main__":
    main()
