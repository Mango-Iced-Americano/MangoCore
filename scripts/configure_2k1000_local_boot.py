#!/usr/bin/env python3
"""Configure safe 2K1000LA local-kernel autoboot with TFTP fallback."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import subprocess
import sys
from typing import Optional

from boot_2k1000_tftp import (
    BootError,
    UBootConsole,
    crc32,
    detect_serial,
    ensure_interface,
    release_matching_screen,
    require,
    validate_uimage,
)


EXPECTED_MODEL = "TS32GMTS400"
EXPECTED_DISK_ID = 0x4D414E47
P3_START_LBA = 0xA80800
P3_SECTORS = 0x180000
LOADADDR = "0x9000000098000000"
LOCAL_KERNEL = "/boot/kernel-A.ui"
TFTP_KERNEL = "kernel-2k1000-mainline.ui"
CONFIRMATION = "2k1000-local-boot"

LOCAL_BOOT = (
    "scsi reset;"
    "mw.l ${loadaddr} 0 1;"
    "ext4load scsi 0:3 ${loadaddr} /boot/kernel-A.ui;"
    "bootm ${loadaddr}"
)
BOOTCMD = "run mango_local_boot; run mango_tftp_boot; run mango_bootcmd_legacy"


def make_tftp_boot(kernel_name: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9._-]+", kernel_name):
        raise BootError(f"unsafe TFTP kernel filename: {kernel_name!r}")
    return (
        "mw.l ${loadaddr} 0 1;"
        f"tftpboot ${{loadaddr}} {kernel_name};"
        "bootm ${loadaddr}"
    )


def quote_env(value: str) -> str:
    if not value or "'" in value or any(ord(char) < 0x20 for char in value):
        raise BootError(f"U-Boot environment value cannot be represented safely: {value!r}")
    return f"'{value}'"


def env_value(output: str, name: str) -> Optional[str]:
    match = re.search(rf"^{re.escape(name)}=(.*)$", output, re.MULTILINE)
    return match.group(1).strip() if match else None


def validate_partition_table(output: str) -> None:
    require(
        rf"^\s*3\s+{P3_START_LBA}\s+{P3_SECTORS}\s+"
        rf"{EXPECTED_DISK_ID:08x}-03\s+83\s*$",
        output,
        "P3 partition identity mismatch",
    )


def set_environment(console: UBootConsole, name: str, value: str) -> None:
    console.command(f"setenv {name} {quote_env(value)}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial")
    parser.add_argument("--interface", default="en8")
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--board-ip", default="192.168.9.20")
    parser.add_argument("--netmask", default="255.255.255.0")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--reset-timeout", type=float, default=120)
    parser.add_argument(
        "--kernel-image",
        type=Path,
        default=Path("build/la64/release/normal/board/2k1000/kernel-2k1000-mainline.ui"),
    )
    parser.add_argument("--tftp-kernel", default=TFTP_KERNEL)
    parser.add_argument(
        "--log", type=Path, default=Path("/private/tmp/mango-2k1000-local-boot.log")
    )
    parser.add_argument("--confirm-saveenv", required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.confirm_saveenv != CONFIRMATION:
        raise BootError(
            f"confirmation mismatch: pass --confirm-saveenv {CONFIRMATION}"
        )
    image = args.kernel_image.expanduser().resolve()
    validate_uimage(image)
    tftp_boot = make_tftp_boot(args.tftp_kernel)

    try:
        import serial
    except ImportError as error:
        raise BootError("pyserial is required") from error

    ensure_interface(args.interface, args.host_ip, args.netmask, True)
    serial_path = detect_serial(args.serial)
    release_matching_screen(serial_path, True)
    console = UBootConsole(serial, serial_path, args.log, args.baud)
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
        validate_partition_table(console.command("scsi part 0", timeout=20))

        loaded = console.command(
            f"ext4load scsi 0:3 {LOADADDR} {LOCAL_KERNEL}", timeout=90
        )
        loaded_bytes = int(
            require(r"(\d+)\s+bytes read", loaded, "local kernel not readable").group(1)
        )
        if loaded_bytes != image.stat().st_size:
            raise BootError(
                f"local kernel size mismatch: {loaded_bytes} != {image.stat().st_size}"
            )
        checksum = console.command(f"crc32 {LOADADDR} 0x{loaded_bytes:x}", timeout=60)
        actual_crc = int(require(r"==>\s*([0-9a-f]{8})", checksum, "CRC missing").group(1), 16)
        expected_crc = crc32(image)
        if actual_crc != expected_crc:
            raise BootError(
                f"local kernel CRC mismatch: {actual_crc:08x} != {expected_crc:08x}"
            )
        info = console.command(f"iminfo {LOADADDR}", timeout=30)
        require(r"LoongArch", info, "local kernel architecture mismatch")
        require(r"Checksum\s+\.\.\.\s+OK", info, "local kernel checksum failed")

        before = console.command(
            "printenv bootcmd bootdelay mango_bootcmd_legacy mango_local_boot mango_tftp_boot"
        )
        legacy = env_value(before, "mango_bootcmd_legacy")
        current = env_value(before, "bootcmd")
        if legacy is None and current and "mango_local_boot" not in current:
            set_environment(console, "mango_bootcmd_legacy", current)

        set_environment(console, "mango_local_boot", LOCAL_BOOT)
        set_environment(console, "mango_tftp_boot", tftp_boot)
        set_environment(console, "bootcmd", BOOTCMD)
        console.command("setenv bootdelay 3")

        staged = console.command(
            "printenv bootcmd bootdelay mango_bootcmd_legacy mango_local_boot mango_tftp_boot"
        )
        if env_value(staged, "bootcmd") != BOOTCMD:
            raise BootError("staged bootcmd verification failed")
        if env_value(staged, "mango_local_boot") != LOCAL_BOOT:
            raise BootError("staged local boot verification failed")
        if env_value(staged, "mango_tftp_boot") != tftp_boot:
            raise BootError("staged TFTP fallback verification failed")
        if env_value(staged, "bootdelay") != "3":
            raise BootError("staged bootdelay verification failed")

        saved = console.command("saveenv", timeout=60)
        require(r"Sav(?:ing|ed) Environment|done", saved, "saveenv did not report success")
        verified = console.command(
            "printenv bootcmd bootdelay mango_bootcmd_legacy mango_local_boot mango_tftp_boot"
        )
        if env_value(verified, "bootcmd") != BOOTCMD:
            raise BootError("saved bootcmd verification failed")
        print(
            f"[local-boot] PASS: {LOCAL_KERNEL} bytes={loaded_bytes} "
            f"crc32={actual_crc:08x} bootdelay=3 TFTP={args.tftp_kernel}",
            flush=True,
        )
    finally:
        console.close()


if __name__ == "__main__":
    try:
        main()
    except (BootError, OSError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
