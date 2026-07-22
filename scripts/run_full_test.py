#!/usr/bin/env python3
"""Canonical full-test entry point; use --dry-run to inspect QEMU commands."""

from full_test.cli import main


if __name__ == "__main__":
    raise SystemExit(main())
