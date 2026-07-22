"""Load canonical QEMU commands from the Make-owned profile definitions."""

from __future__ import annotations

import shlex
import subprocess

from image_roles import ImageRoles


PROFILES = (
    "normal",
    "competition",
    "derived-competition",
    "development",
    "debug",
    "regression",
    "ktest",
)



class QemuProfileError(RuntimeError):
    """Raised when Make cannot render a requested canonical profile."""


def build_qemu_args(roles: ImageRoles, arch: str, profile_name: str) -> tuple[str, ...]:
    """Return the canonical launch arguments for every supported QEMU profile."""
    if arch not in {"rv64", "la64"}:
        raise QemuProfileError(f"unsupported QEMU architecture: {arch}")
    if profile_name not in PROFILES:
        raise QemuProfileError(f"unsupported QEMU profile: {profile_name}")
    completed = subprocess.run(
        ["make", "-s", "-f", f"make/{arch}.mk", "qemu-profile-dry-run", f"QEMU_PROFILE={profile_name}"],
        cwd=roles.repository / "os",
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise QemuProfileError(completed.stderr.strip() or f"Make failed to render {arch} {profile_name}")
    command = completed.stdout.strip()
    if not command or "\n" in command:
        raise QemuProfileError(f"Make emitted an invalid QEMU command for {arch} {profile_name}")
    return tuple(shlex.split(command))


def render_qemu_command(roles: ImageRoles, arch: str, profile_name: str) -> str:
    """Render a shell-safe canonical command for dry-run evidence."""
    return shlex.join(build_qemu_args(roles, arch, profile_name))


def build_rv64_cmd(roles: ImageRoles) -> str:
    """Retain the runner's RV64 normal-profile constructor."""
    return render_qemu_command(roles, "rv64", "normal")


def build_la64_cmd(roles: ImageRoles) -> str:
    """Retain the runner's LA64 normal-profile constructor."""
    return render_qemu_command(roles, "la64", "normal")
