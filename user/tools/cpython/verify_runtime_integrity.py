#!/usr/bin/env python3
"""Verify the extracted strict runtime's complete native ELF closure."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent
MANIFEST = ROOT / "strict-runtime-manifest.json"
EXPECTED_INTERP = "/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
assert manifest["runtime_policy"] == "mangocore-la64-strict-align-v1"
assert manifest["runtime_interpreter"] == EXPECTED_INTERP
elfs = manifest.get("elfs")
assert isinstance(elfs, list) and elfs
for item in elfs:
    path = ROOT / item["path"]
    assert path.is_file() and not path.is_symlink(), path
    actual = sha256(path)
    assert actual == item["sha256"], (path, actual, item["sha256"])
    interpreter = item.get("interpreter")
    assert interpreter in (None, EXPECTED_INTERP), (path, interpreter)
print(f"strict-runtime-integrity-ok elfs={len(elfs)}")
