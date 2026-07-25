"""CLI boundary for canonical, dry-run, and fixture full-test paths."""

from __future__ import annotations

import argparse
import os
from pathlib import Path

from image_roles import load_roles

from .commands import PROFILES, render_qemu_command
from .runner import run_fixture, run_full_test


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="print canonical commands without building or launching QEMU")
    parser.add_argument("--serial", action="store_true", help="run RV64 QEMU then LA64 QEMU sequentially (default: parallel, build is always serial)")
    parser.add_argument("--fixture", choices=("abnormal-signature", "build-failure", "extraction-failure", "qemu-timeout", "missing-terminal-marker", "judge-missing-group", "judge-nonzero", "qemu-nonzero"))
    return parser


def main() -> int:
    """Parse CLI input once and dispatch to a typed orchestration path."""
    args = _parser().parse_args()
    root = Path(__file__).resolve().parents[2]
    roles = load_roles(root)
    if args.fixture is not None:
        return run_fixture(root, args.fixture)
    if args.dry_run:
        for profile in PROFILES:
            for arch in ("rv64", "la64"):
                print(f"{profile} {arch}: {render_qemu_command(roles, arch, profile)}")
        return 0
    return run_full_test(root, roles, int(os.environ.get("QEMU_TIMEOUT", "7200")), args.serial)
