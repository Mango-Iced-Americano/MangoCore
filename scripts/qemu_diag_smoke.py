#!/usr/bin/env python3
"""Boot a perf_diag QEMU image in shell mode and run diag_smoke_test.sh."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import pexpect


ROOT = Path(__file__).resolve().parents[1]
DOCKER_IMAGE = "zhouzhouyi/os-contest:20260104"


def qemu_args(args: argparse.Namespace) -> list[str]:
    prefix = [
        "run",
        "--rm",
        "-i",
        "-v",
        f"{ROOT}:/app",
        "-w",
        "/app/os",
        DOCKER_IMAGE,
        "timeout",
        str(args.timeout + 30),
    ]
    if args.arch == "rv64":
        return prefix + [
            "qemu-system-riscv64",
            "-machine", "virt",
            "-kernel", args.kernel,
            "-m", "1024",
            "-nographic",
            "-smp", "1",
            "-bios", "default",
            "-drive", f"file={args.sdcard},if=none,format=raw,id=x0",
            "-device", "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0",
            "-drive", f"file={args.disk},if=none,format=raw,id=x1",
            "-device", "virtio-blk-device,drive=x1,bus=virtio-mmio-bus.1",
            "-device", "virtio-rng-device,bus=virtio-mmio-bus.2",
            "-no-reboot",
        ]
    return prefix + [
        "qemu-system-loongarch64",
        "-machine", "virt",
        "-kernel", args.kernel,
        "-m", "1G",
        "-nographic",
        "-smp", "1",
        "-drive", f"file={args.sdcard},if=none,format=raw,id=x0",
        "-device", "virtio-blk-pci,drive=x0",
        "-drive", f"file={args.disk},if=none,format=raw,id=x1",
        "-device", "virtio-blk-pci,drive=x1",
        "-device", "virtio-rng-pci",
        "-no-reboot",
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=("rv64", "la64"), required=True)
    parser.add_argument("--kernel", required=True)
    parser.add_argument("--sdcard", required=True)
    parser.add_argument("--disk", required=True)
    parser.add_argument("--timeout", type=int, default=240)
    args = parser.parse_args()

    child = pexpect.spawn(
        "docker",
        qemu_args(args),
        cwd=str(ROOT),
        encoding="utf-8",
        codec_errors="replace",
        timeout=args.timeout,
        echo=False,
    )
    child.logfile_read = sys.stdout
    failed = False
    try:
        child.expect_exact("[initproc] entering shell mode")
        child.expect([r"/ # ", r"# "])
        child.sendline("sh /sdcard/diag_smoke_test.sh")
        result = child.expect_exact(["FAIL:", "=== DIAG SMOKE TEST DONE ==="])
        failed = result == 0
        child.sendline("exit")
        child.expect(pexpect.EOF, timeout=30)
    except (pexpect.TIMEOUT, pexpect.EOF) as exc:
        print(f"\n[qemu-diag-smoke] console failure: {exc}", file=sys.stderr)
        failed = True
        child.send("\x01x")
    finally:
        if child.isalive():
            child.close(force=True)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
