#!/usr/bin/env python3
"""Publish the strict-aligned CPython runtime as the board's P4 default."""

from __future__ import annotations

import argparse
import hashlib
import json
import shlex
import subprocess
import sys
import tarfile
import time
from pathlib import Path, PurePosixPath

from kernel_perf import SERIAL_COMMAND_LIMIT, serial_work_line_size


ROOT = Path(__file__).resolve().parents[1]
HARNESS = ROOT / "scripts" / "kernel_perf.py"
POLICY = "mangocore-la64-strict-align-v1"
TARGET = "loongarch64-linux-musl"
REQUIRED_FLAGS = {"-march=loongarch64", "-mabi=lp64d", "-mstrict-align"}
RUNTIME_INTERP = "/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1"
REQUIRED_MEMBERS = {
    "lib/ld-musl-loongarch64.so.1",
    "usr/bin/python3",
    "python3-wrapper.sh",
    "strict_runtime_smoke.sh",
    "verify_runtime_integrity.py",
    "strict-runtime-manifest.json",
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def http_server_command(directory: Path, host: str, port: int) -> list[str]:
    ruby = Path("/usr/bin/ruby")
    if sys.platform == "darwin" and ruby.is_file():
        return [
            str(ruby),
            "-run",
            "-e",
            "httpd",
            str(directory),
            "-p",
            str(port),
            "-b",
            host,
        ]
    return [
        sys.executable,
        "-m",
        "http.server",
        str(port),
        "--bind",
        host,
        "--directory",
        str(directory),
    ]


def validate_archive(path: Path) -> str:
    def stays_below_root(candidate: PurePosixPath) -> bool:
        depth = 0
        for part in candidate.parts:
            if part in ("", "."):
                continue
            if part == "..":
                if depth == 0:
                    return False
                depth -= 1
            else:
                depth += 1
        return True

    with tarfile.open(path, "r:xz") as archive:
        members: dict[str, tarfile.TarInfo] = {}
        for member in archive.getmembers():
            name = PurePosixPath(member.name)
            if name.is_absolute() or not stays_below_root(name):
                raise SystemExit("unsafe archive member: %s" % member.name)
            if not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
                raise SystemExit("unsupported special archive member: %s" % member.name)
            if member.issym() or member.islnk():
                target = PurePosixPath(member.linkname)
                if target.is_absolute():
                    raise SystemExit("absolute archive link: %s" % member.name)
                resolved = name.parent / target if member.issym() else target
                if not stays_below_root(resolved):
                    raise SystemExit("escaping archive link: %s" % member.name)
            normalized = name.as_posix()
            if normalized in members:
                raise SystemExit("duplicate archive member: %s" % member.name)
            members[normalized] = member

        missing = sorted(REQUIRED_MEMBERS - members.keys())
        if missing:
            raise SystemExit("runtime archive is incomplete: " + ", ".join(missing))
        manifest_stream = archive.extractfile(members["strict-runtime-manifest.json"])
        if manifest_stream is None:
            raise SystemExit("cannot read strict runtime manifest")
        manifest_bytes = manifest_stream.read()
        try:
            manifest = json.loads(manifest_bytes)
        except json.JSONDecodeError as exc:
            raise SystemExit("invalid strict runtime manifest: %s" % exc) from exc
        flags = set(str(manifest.get("strict_flags", "")).split())
        elfs = manifest.get("elfs")
        if (
            manifest.get("target") != TARGET
            or not REQUIRED_FLAGS.issubset(flags)
            or manifest.get("pgo") is not True
            or manifest.get("lto") is not True
            or manifest.get("runtime_interpreter") != RUNTIME_INTERP
            or not isinstance(elfs, list)
            or not elfs
            or manifest.get("elf_count") != len(elfs)
        ):
            raise SystemExit("runtime manifest does not describe the required strict build")
        for entry in elfs:
            if not isinstance(entry, dict):
                raise SystemExit("invalid strict runtime ELF entry")
            member = members.get(str(entry.get("path", "")))
            stream = archive.extractfile(member) if member is not None else None
            if stream is None:
                raise SystemExit("missing manifest ELF: %s" % entry.get("path", ""))
            actual = hashlib.sha256(stream.read()).hexdigest()
            if actual != entry.get("sha256"):
                raise SystemExit("manifest ELF hash mismatch: %s" % entry.get("path", ""))
            interpreter = entry.get("interpreter")
            if interpreter not in (None, RUNTIME_INTERP):
                raise SystemExit(
                    "manifest ELF has non-P4 interpreter: %s" % entry.get("path", "")
                )
        return hashlib.sha256(manifest_bytes).hexdigest()


def board_phase(args: argparse.Namespace, name: str, command: str, timeout: int) -> int:
    argv = [
        sys.executable,
        str(HARNESS),
        "board",
        "--run-dir",
        str(args.run_dir),
        "--serial",
        args.serial,
        "--baud",
        str(args.baud),
        "--test",
        name,
        "--build-mode",
        args.build_mode,
        "--cache-state",
        "deployment",
        "--timeout",
        str(timeout),
        "--command",
        command,
    ]
    if args.quiet:
        argv.append("--quiet")
    return subprocess.run(argv, cwd=ROOT, check=False).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    artifact_source = parser.add_mutually_exclusive_group(required=True)
    artifact_source.add_argument("--artifact", type=Path)
    artifact_source.add_argument(
        "--artifact-index",
        type=Path,
        help="verified current.json emitted by the strict runtime build",
    )
    parser.add_argument("--serial", default="/dev/cu.wchusbserial120")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--port", type=int, default=8009)
    parser.add_argument(
        "--target-parent",
        default="/persist/python-runtime/releases",
        help="P4 directory containing immutable strict runtime releases",
    )
    parser.add_argument(
        "--current-link",
        default="/persist/python-runtime/current",
        help="stable P4 symlink used by every LA64 Python entry point",
    )
    parser.add_argument(
        "--build-mode",
        choices=("production", "diag_off", "diag_on"),
        default="diag_on",
    )
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    index: dict[str, object] | None = None
    if args.artifact_index is not None:
        index_path = args.artifact_index.resolve()
        try:
            index = json.loads(index_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            parser.error("cannot read artifact index %s: %s" % (index_path, exc))
        if index.get("runtime_policy") != POLICY:
            parser.error("artifact index does not select the strict LA64 runtime")
        artifact_name = str(index.get("artifact", ""))
        if PurePosixPath(artifact_name).name != artifact_name:
            parser.error("artifact index contains an unsafe filename")
        artifact = (index_path.parent / artifact_name).resolve()
    else:
        assert args.artifact is not None
        artifact = args.artifact.resolve()
    if not artifact.is_file():
        parser.error("artifact does not exist: %s" % artifact)
    if not args.run_dir.is_dir() and not args.dry_run:
        parser.error("run directory does not exist: %s" % args.run_dir)
    raw_parent = args.target_parent.rstrip("/")
    parent_path = PurePosixPath(raw_parent)
    if parent_path != PurePosixPath("/persist/python-runtime/releases"):
        parser.error("target parent must be /persist/python-runtime/releases")
    parent = str(parent_path)
    current_path = PurePosixPath(args.current_link.rstrip("/"))
    if (
        not current_path.is_absolute()
        or current_path.parts[:3] != ("/", "persist", "python-runtime")
        or current_path.name != "current"
        or ".." in current_path.parts
    ):
        parser.error("current link must be /persist/python-runtime/current")
    current = str(current_path)

    manifest_digest = validate_archive(artifact)
    digest = sha256(artifact)
    if index is not None:
        if digest != index.get("sha256"):
            parser.error("artifact sha256 does not match its index")
        if manifest_digest != index.get("manifest_sha256"):
            parser.error("manifest sha256 does not match its index")
    short = digest[:12]
    final = "%s/%s" % (parent, short)
    staging = "%s/.%s.staging" % (parent, short)
    # Keep the compressed transfer object on disposable tmpfs.  P4 only needs
    # enough free blocks for the immutable extracted release; placing both the
    # archive and staging tree on P4 needlessly raises peak usage by ~80 MiB.
    # P2 /scratch is intentionally not used here: real-board measurement shows
    # its current sequential-write path is far slower than the P4 deployment.
    remote_archive = "/tmp/.%s.part" % artifact.name
    url = "http://%s:%d/%s" % (args.host_ip, args.port, artifact.name)

    marker = ".mango-strict-runtime"
    q = shlex.quote
    # This controller is itself hash-checked before execution.  Every board
    # operation uses BusyBox or the just-verified strict runtime; /tools is not
    # part of the bootstrap chain.
    control_script = """#!/bin/sh
set -eu
echo '[python-deploy] stage=p4-preflight'
/bin/busybox grep -Eq '^/persist[[:space:]]+/persist[[:space:]]+ext4[[:space:]]+rw([,[:space:]])' /proc/mounts
test ! -L /persist
test -w /persist
test "$(blockdev --getsize64 /dev/sda4)" = 4294967296
test ! -L /tmp
test -w /tmp
parent=%s
current=%s
final=%s
staging=%s
archive=%s
mkdir -p "$parent"
test "$(/bin/busybox readlink -f "$parent")" = "$parent"
test ! -e "$current" || test -L "$current"
new_release=0
cleanup_failed_release() {
    rc=$?
    trap - EXIT INT TERM
    if test "$rc" -ne 0 && test "$new_release" = 1; then
        echo '[python-deploy] stage=cleanup-failed-release'
        rm -rf "$staging"
        rm -f "$archive"
        sync
    fi
    exit "$rc"
}
trap cleanup_failed_release EXIT INT TERM
if test ! -d "$final"; then
    echo '[python-deploy] stage=download'
    new_release=1
    rm -rf "$staging"
    rm -f "$archive"
    mkdir "$staging"
    /bin/busybox wget -O "$archive" %s
    printf '%%s  %%s\n' %s "$archive" | /bin/busybox sha256sum -c -
    echo '[python-deploy] stage=extract'
    /bin/busybox tar -xJf "$archive" -C "$staging"
    test -x "$staging/lib/ld-musl-loongarch64.so.1"
    test -x "$staging/usr/bin/python3"
    test -r "$staging/strict-runtime-manifest.json"
    sync
    mv "$staging" "$final"
    sync
fi
echo '[python-deploy] stage=identity'
set -- $(/bin/busybox sha256sum "$final/strict-runtime-manifest.json")
test "$1" = %s
printf '%%s\n' schema=1 runtime_policy=%s artifact_sha256=%s manifest_sha256=%s >"$final/%s.tmp"
mv -f "$final/%s.tmp" "$final/%s"
sync
echo '[python-deploy] stage=smoke'
if ! "$final/strict_runtime_smoke.sh"; then
    test "$new_release" = 0 || rm -rf "$final"
    exit 1
fi
echo '[python-deploy] stage=publish-current'
tmp=/persist/python-runtime/.current-%s
rm -f "$tmp"
ln -s releases/%s "$tmp"
sync
CPYTHON_ROOT="$final" "$final/python3-wrapper.sh" -S -c \
    'import os,sys; os.replace(sys.argv[1], sys.argv[2])' "$tmp" "$current"
sync
test "$(/bin/busybox readlink -f "$current")" = "$final"
mkdir -p /persist/python/tmp /persist/python/user /persist/python/pycache
rm -f "$archive"
trap - EXIT INT TERM
echo '[python-deploy] PASS current='"$final"
""" % (
        q(parent),
        q(current),
        q(final),
        q(staging),
        q(remote_archive),
        q(url),
        q(digest),
        q(manifest_digest),
        q(POLICY),
        q(digest),
        q(manifest_digest),
        q(marker),
        q(marker),
        q(marker),
        q(short),
        q(short),
    )
    control_name = "pyd-%s.sh" % short
    control_path = artifact.parent / control_name
    control_digest = hashlib.sha256(control_script.encode("utf-8")).hexdigest()
    remote_control = "/tmp/pyd"
    control_url = "http://%s:%d/%s" % (args.host_ip, args.port, control_name)
    phases = (
        (
            "pyctl_get",
            "/bin/busybox wget -O %s %s"
            % tuple(map(q, (remote_control, control_url))),
            120,
        ),
        (
            "pyctl_sha",
            "printf '%s  %s\\n'|/bin/busybox sha256sum -c -"
            % (control_digest, remote_control),
            120,
        ),
        ("pyctl_run", "/bin/sh %s" % q(remote_control), 1800),
        ("pyctl_rm", "rm -f %s" % q(remote_control), 120),
    )

    print("artifact_sha256=%s" % digest)
    print("release=%s" % final)
    print("current=%s" % current)
    for name, command, _ in phases:
        wrapped_bytes = serial_work_line_size(command, name)
        if wrapped_bytes > SERIAL_COMMAND_LIMIT:
            raise SystemExit(
                "wrapped board command exceeds %d bytes: %s (%d)"
                % (SERIAL_COMMAND_LIMIT, name, wrapped_bytes)
            )
        if args.dry_run:
            print(
                "%s command_bytes=%d wrapped_bytes=%d"
                % (name, len(command.encode("utf-8")), wrapped_bytes)
            )
            print(command)
    if args.dry_run:
        print("control_script_bytes=%d" % len(control_script.encode("utf-8")))
        print("control_script_sha256=%s" % control_digest)
        return 0

    if control_path.exists():
        raise SystemExit("temporary controller already exists: %s" % control_path)
    control_path.write_text(control_script, encoding="utf-8")
    server: subprocess.Popen[bytes] | None = None
    try:
        server = subprocess.Popen(
            http_server_command(artifact.parent, args.host_ip, args.port),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        time.sleep(0.3)
        if server.poll() is not None:
            raise SystemExit("artifact HTTP server failed to start")
        for name, command, timeout in phases:
            if board_phase(args, name, command, timeout):
                return 1
        return 0
    finally:
        if server is not None and server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=2)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=2)
        control_path.unlink(missing_ok=True)


if __name__ == "__main__":
    raise SystemExit(main())
