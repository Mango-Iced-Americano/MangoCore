#!/usr/bin/env python3
"""Build an MBR-partitioned tools disk image.

Layout:
  MBR sector 0
  gap: 1 MiB (2048 sectors)
  partition 1: 768 MiB, type 0x83 — tools files (payload from existing ext4 image)
  partition 2: 1280 MiB, type 0x0C — vfat for oscomp mount test
  total: 2049 MiB

Usage:
  python3 make_mbr_tools_disk.py <tools_payload.img> <output_disk.img>
"""

import argparse
import os
import struct
import subprocess
import sys

SECTOR = 512
MIB = 1024 * 1024

P1_START = 2048               # 1 MiB alignment
P1_MIB = 768
P2_MIB = 1280

P1_SECTORS = P1_MIB * MIB // SECTOR
P2_START = P1_START + P1_SECTORS
P2_SECTORS = P2_MIB * MIB // SECTOR
TOTAL_SECTORS = P2_START + P2_SECTORS


def put_partition_entry(mbr: bytearray, index: int, part_type: int,
                        start_lba: int, sectors: int) -> None:
    off = 446 + index * 16
    mbr[off + 0] = 0x00            # boot flag
    mbr[off + 1] = 0xFE            # start CHS (dummy)
    mbr[off + 2] = 0xFF
    mbr[off + 3] = 0xFF
    mbr[off + 4] = part_type
    mbr[off + 5] = 0xFE            # end CHS (dummy)
    mbr[off + 6] = 0xFF
    mbr[off + 7] = 0xFF
    struct.pack_into("<I", mbr, off + 8, start_lba)
    struct.pack_into("<I", mbr, off + 12, sectors)


def main() -> None:
    ap = argparse.ArgumentParser(
        description="Build MBR-partitioned tools disk image")
    ap.add_argument("tools_img", help="Existing tools ext4 payload image")
    ap.add_argument("out_img", help="Output disk image path")
    args = ap.parse_args()

    tools_size = os.path.getsize(args.tools_img)
    max_p1_bytes = P1_SECTORS * SECTOR
    if tools_size > max_p1_bytes:
        print(f"ERROR: tools image ({tools_size} bytes) does not fit in "
              f"partition 1 ({max_p1_bytes} bytes / {P1_MIB} MiB)",
              file=sys.stderr)
        sys.exit(1)

    total_bytes = TOTAL_SECTORS * SECTOR

    with open(args.out_img, "wb") as out:
        # Create sparse image
        out.truncate(total_bytes)

        # Write MBR at sector 0
        mbr = bytearray(512)
        put_partition_entry(mbr, 0, 0x83, P1_START, P1_SECTORS)
        put_partition_entry(mbr, 1, 0x0C, P2_START, P2_SECTORS)
        mbr[510] = 0x55
        mbr[511] = 0xAA

        out.seek(0)
        out.write(mbr)

        # Copy tools payload into partition 1
        out.seek(P1_START * SECTOR)
        with open(args.tools_img, "rb") as src:
            while True:
                chunk = src.read(MIB)
                if not chunk:
                    break
                out.write(chunk)

    print(f"[mbr-disk] {args.out_img}: {TOTAL_SECTORS} sectors "
          f"({TOTAL_SECTORS * SECTOR // MIB} MiB)")
    print(f"  partition 1: start={P1_START} sectors={P1_SECTORS} "
          f"({P1_MIB} MiB) type=0x83")
    print(f"  partition 2: start={P2_START} sectors={P2_SECTORS} "
          f"({P2_MIB} MiB) type=0x0C")

    # Format partition 2 as FAT32 for oscomp mount test
    p2_blocks = P2_SECTORS * SECTOR // 1024
    cmd = ["mkfs.vfat", "-F", "32", "--offset", str(P2_START),
           args.out_img, str(p2_blocks)]
    print(f"[mbr-disk] formatting partition 2: {' '.join(cmd)}")
    subprocess.run(cmd, check=True)


if __name__ == "__main__":
    main()
