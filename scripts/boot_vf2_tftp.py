#!/usr/bin/env python3
"""Fail closed: physical-board deployment is an approval-gated human procedure."""

raise SystemExit(
    "scripts/boot_vf2_tftp.py is disabled: do not deploy, reset, use TFTP, "
    "or boot a board from this workspace. Follow docs/08_testing/vf2-booti-handoff.md "
    "only after explicit human approval."
)
