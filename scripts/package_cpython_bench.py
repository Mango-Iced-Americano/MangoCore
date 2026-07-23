#!/usr/bin/env python3
"""Build a deterministic benchmark-only ZIP without the CPython runtime."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "user" / "tools" / "cpython"
DEFAULT_OUTPUT = ROOT / "target" / "cpython-bench" / "cpython-bench-suite.zip"
BUNDLE_ROOT = "mangocore-cpython-bench-suite"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def git_head() -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.stdout.strip() or "unknown"


def source_files() -> list[Path]:
    files = [SOURCE / "cpython_benchmark.sh"]
    files.extend(
        path
        for path in (SOURCE / "bench").rglob("*")
        if path.is_file()
        and "__pycache__" not in path.parts
        and path.suffix in (".py", ".md", ".txt")
    )
    return sorted(files, key=lambda path: path.relative_to(SOURCE).as_posix())


def zip_info(name: str, mode: int = 0o644) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.compress_type = zipfile.ZIP_STORED
    info.create_system = 3
    info.external_attr = (mode & 0xFFFF) << 16
    return info


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()

    files = source_files()
    manifest_files = []
    payloads = []
    for path in files:
        relative = path.relative_to(SOURCE).as_posix()
        data = path.read_bytes()
        payloads.append((relative, data, 0o755 if path.name.endswith(".sh") else 0o644))
        manifest_files.append({"path": relative, "bytes": len(data), "sha256": sha256(data)})

    manifest = {
        "schema": 1,
        "role": "mangocore-cpython-bench-suite",
        "git_head": git_head(),
        "files": manifest_files,
    }
    manifest_data = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("utf-8")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(args.output, "w") as archive:
        for relative, data, mode in payloads:
            archive.writestr(zip_info(f"{BUNDLE_ROOT}/{relative}", mode), data)
        archive.writestr(
            zip_info(f"{BUNDLE_ROOT}/bundle_manifest.json"),
            manifest_data,
        )

    bundle_hash = sha256(args.output.read_bytes())
    print(
        json.dumps(
            {
                "output": str(args.output.resolve()),
                "bytes": args.output.stat().st_size,
                "sha256": bundle_hash,
                "files": len(files),
                "git_head": manifest["git_head"],
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
