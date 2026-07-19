#!/usr/bin/env python3
"""Install the reviewed DDGS 9.0.0 redirect fix as a P4 user overlay."""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
from pathlib import Path


EXPECTED_VERSION = "9.0.0"
SOURCE_SHA256 = "eb9a3cc9bcd06f2d711d2a736e7758bd68ebcb46458883d6c183eeb62c383db2"
PATCHED_SHA256 = "3c321b9445ec57db0bd1d06899c6a10eeeea2817fa7ecbc1b2e08f37878bed24"
OLD = b"            follow_redirects=False,\n"
NEW = b"            follow_redirects=True,\n"
PACKAGE_FILES = (
    "__init__.py",
    "__main__.py",
    "cli.py",
    "ddgs.py",
    "exceptions.py",
    "py.typed",
    "utils.py",
    "version.py",
)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def ensure_beneath(path: Path, root: Path, label: str) -> None:
    if path.is_symlink():
        raise RuntimeError(f"{label} must not be a symlink: {path}")
    if path.resolve() != root.resolve() and root.resolve() not in path.resolve().parents:
        raise RuntimeError(f"{label} escapes its root: {path}")


def metadata_version(site: Path) -> str:
    matches = sorted(site.glob("ddgs-*.dist-info/METADATA"))
    if len(matches) != 1:
        raise RuntimeError(f"expected one DDGS metadata file under {site}, found {len(matches)}")
    for line in matches[0].read_text(encoding="utf-8").splitlines():
        if line.startswith("Version: "):
            return line.removeprefix("Version: ").strip()
    raise RuntimeError("DDGS metadata has no Version field")


def verify_package(package: Path, expected_ddgs_sha: str) -> None:
    ensure_beneath(package, package.parent, "DDGS package")
    if not package.is_dir():
        raise RuntimeError(f"DDGS package is missing: {package}")
    for name in PACKAGE_FILES:
        source = package / name
        ensure_beneath(source, package, f"DDGS {name}")
        if not source.is_file():
            raise RuntimeError(f"DDGS package file is missing: {source}")
    actual = digest((package / "ddgs.py").read_bytes())
    if actual != expected_ddgs_sha:
        raise RuntimeError(f"unexpected DDGS source digest: {actual}")


def patched_source(original: bytes) -> bytes:
    if digest(original) != SOURCE_SHA256:
        raise RuntimeError("refusing to patch unreviewed DDGS 9.0.0 source")
    if original.count(OLD) != 1:
        raise RuntimeError("reviewed DDGS redirect setting was not found exactly once")
    patched = original.replace(OLD, NEW, 1)
    if digest(patched) != PATCHED_SHA256:
        raise RuntimeError("constructed DDGS redirect patch has an unexpected digest")
    return patched


def install(runtime_site: Path, user_site: Path, check: bool) -> str:
    ensure_beneath(runtime_site, runtime_site, "runtime site")
    ensure_beneath(user_site, user_site, "user site")
    if metadata_version(runtime_site) != EXPECTED_VERSION:
        raise RuntimeError(f"only DDGS {EXPECTED_VERSION} is reviewed")

    runtime_package = runtime_site / "ddgs"
    runtime_source = runtime_package / "ddgs.py"
    runtime_digest = digest(runtime_source.read_bytes())
    overlay = user_site / "ddgs"

    if overlay.exists() or overlay.is_symlink():
        verify_package(overlay, PATCHED_SHA256)
        return "overlay-verified"
    if runtime_digest == PATCHED_SHA256:
        verify_package(runtime_package, PATCHED_SHA256)
        return "runtime-verified"
    verify_package(runtime_package, SOURCE_SHA256)
    if check:
        raise RuntimeError("reviewed DDGS redirect overlay is not installed")

    user_site.mkdir(parents=True, exist_ok=True)
    ensure_beneath(user_site, user_site, "user site")
    temporary = user_site / f".ddgs-mango-{os.getpid()}"
    if temporary.exists() or temporary.is_symlink():
        raise RuntimeError(f"temporary DDGS overlay already exists: {temporary}")
    temporary.mkdir(mode=0o755)
    try:
        for name in PACKAGE_FILES:
            source = runtime_package / name
            target = temporary / name
            shutil.copyfile(source, target)
            target.chmod(0o644)
        (temporary / "ddgs.py").write_bytes(patched_source(runtime_source.read_bytes()))
        (temporary / "ddgs.py").chmod(0o644)
        verify_package(temporary, PATCHED_SHA256)
        os.rename(temporary, overlay)
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)
    verify_package(overlay, PATCHED_SHA256)
    return "overlay-installed"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument(
        "--runtime-site",
        type=Path,
        default=Path(os.environ.get("CPYTHON_ROOT", "/persist/python-runtime/current"))
        / "usr/lib/python3.14/site-packages",
    )
    parser.add_argument(
        "--user-site",
        type=Path,
        default=Path("/persist/python/user/lib/python3.14/site-packages"),
    )
    args = parser.parse_args()
    try:
        status = install(args.runtime_site, args.user_site, args.check)
    except Exception as exc:
        parser.exit(1, f"[ddgs-redirect] FAIL: {exc}\n")
    print(
        f"[ddgs-redirect] status={status} version={EXPECTED_VERSION} "
        f"user_site={args.user_site}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
