#!/usr/bin/env python3
"""
Kernel Development MCP Server for oskernel2026-mango.

Wraps the full kernel dev workflow:
  - Build (kernel-only / full)
  - Run QEMU (with log levels)
  - Test config management (mask, ltp_include, inject)
  - Full test suite (run_full_test.py)
  - GDB interactive debugging (QEMU -s -S + cross-gdb)

All commands execute inside the Docker container via `docker exec`.
"""

import subprocess
import json
import os
import time
import sys
import threading
import re
import signal
from pathlib import Path
from typing import Any

from mcp.server import Server, NotificationOptions, InitializationOptions
from mcp.server.stdio import stdio_server
from mcp.types import Tool, TextContent, ServerCapabilities

# ─── Constants ───────────────────────────────────────────────────────────────
CONTAINER = "oskernel2026-mango-os-dev-1"
CONTAINER_WORKDIR = "/app"
PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent

# Cross-debugger paths (inside Docker container)
GDB_RV = "gdb-multiarch"  # Ubuntu GDB 15+ with multi-arch (supports riscv:rv64)
GDB_LA = "gdb-multiarch"  # same binary, supports loongarch via set architecture

# QEMU connection settings
QEMU_GDB_PORT = 1234

# Active debug sessions: session_id -> {"gdb_proc": Popen, "qemu_proc": Popen | None}
_debug_sessions: dict[str, dict] = {}


# ─── Helpers ──────────────────────────────────────────────────────────────────

def _docker_exec(cmd: str, timeout: int = 120, workdir: str = CONTAINER_WORKDIR) -> tuple[int, str, str]:
    """Run a command inside the Docker container and return (exit_code, stdout, stderr)."""
    full_cmd = ["docker", "exec", "-w", workdir, CONTAINER, "bash", "-c", cmd]
    try:
        r = subprocess.run(full_cmd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout.strip(), r.stderr.strip()
    except subprocess.TimeoutExpired:
        return -1, "", f"Command timed out after {timeout}s"


def _docker_exec_live(cmd: str, timeout: int = 300, workdir: str = CONTAINER_WORKDIR) -> tuple[int, str]:
    """Run a command with real-time stdout streaming (for long builds). Returns (exit_code, combined_output)."""
    full_cmd = ["docker", "exec", "-w", workdir, CONTAINER, "bash", "-c", cmd]
    output_parts: list[str] = []

    try:
        proc = subprocess.Popen(full_cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        assert proc.stdout is not None
        start = time.time()
        for line in proc.stdout:
            elapsed = time.time() - start
            if elapsed > timeout:
                proc.kill()
                output_parts.append(f"\n[TIMEOUT after {timeout}s]")
                break
            output_parts.append(line.rstrip())
        proc.wait()
        return proc.returncode, "\n".join(output_parts)
    except Exception as e:
        return -1, str(e)


def _check_docker() -> bool:
    """Verify the Docker container is running."""
    code, out, _ = _docker_exec("echo ok", timeout=5)
    return code == 0 and "ok" in out


# ─── Build Operations ─────────────────────────────────────────────────────────

def build_kernel(arch: str, log: str = "off") -> dict:
    """Compile kernel only (fast, for iteration)."""
    if arch not in ("rv64", "la64"):
        return {"ok": False, "error": f"Unknown arch: {arch}. Use 'rv64' or 'la64'."}

    cmd = f"cd os && LOG={log} BLK_MODE={'virt_pci' if arch == 'la64' else 'virt'} make {arch}-kernel-build-only"
    code, output = _docker_exec_live(cmd, timeout=300)

    return {
        "ok": code == 0,
        "exit_code": code,
        "output": output[-3000:] if len(output) > 3000 else output,  # tail for large output
    }


def build_all(arch: str, log: str = "off") -> dict:
    """Compile kernel + user programs + filesystem image (full build)."""
    if arch not in ("rv64", "la64"):
        return {"ok": False, "error": f"Unknown arch: {arch}. Use 'rv64' or 'la64'."}

    cmd = f"cd os && LOG={log} make {arch}-only"
    code, output = _docker_exec_live(cmd, timeout=600)

    return {
        "ok": code == 0,
        "exit_code": code,
        "output": output[-5000:] if len(output) > 5000 else output,
    }


# ─── QEMU Operations ──────────────────────────────────────────────────────────

def run_qemu(arch: str, log: str = "off", timeout: int = 300) -> dict:
    """Build (if needed) and run QEMU with full test image. Returns QEMU output."""
    if arch not in ("rv64", "la64"):
        return {"ok": False, "error": f"Unknown arch: {arch}. Use 'rv64' or 'la64'."}

    cmd = f"cd os && LOG={log} make {arch}-run"
    code, output = _docker_exec_live(cmd, timeout=timeout)

    return {
        "ok": code == 0,
        "exit_code": code,
        "output": output[-8000:] if len(output) > 8000 else output,
    }


# ─── Test Config ──────────────────────────────────────────────────────────────

def inject_test_config(arch: str, mask: str = "0x001", ltp_runner: str = "script",
                       ltp_include: str = "", ltp_libc: str = "both",
                       ltp_suites: str = "") -> dict:
    """Modify os_test.conf and inject it into the test image."""
    if arch not in ("rv64", "la64"):
        return {"ok": False, "error": f"Unknown arch: {arch}. Use 'rv64' or 'la64'."}

    blk_mode = "virt_pci" if arch == "la64" else "virt"

    # Write config to temporary file in project root (host side)
    conf_content = f"""mode=run
mask={mask}
ltp_runner={ltp_runner}
ltp_libc={ltp_libc}
ltp_suites={ltp_suites}
ltp_exclude=
ltp_exclude_musl=
ltp_exclude_glibc=
ltp_include={ltp_include}
ltp_from=
"""

    # Write to a temp path that Docker can access (project root is mounted)
    conf_path = f"{CONTAINER_WORKDIR}/os_test.conf"
    # Write via docker exec
    escaped = conf_content.replace("'", "'\\''")
    cmd = f"cat > {conf_path} << 'CONFEOF'\n{conf_content}\nCONFEOF"
    code, out, err = _docker_exec(cmd, timeout=10)

    if code != 0:
        return {"ok": False, "error": f"Failed to write config: {err}"}

    # Inject
    cmd = f"cd os && make conf-inject CONF_ARCH={arch} CONF_BLK_MODE={blk_mode} CONF_FILE=../os_test.conf"
    code, output = _docker_exec_live(cmd, timeout=120)

    return {
        "ok": code == 0,
        "exit_code": code,
        "output": output[-2000:] if len(output) > 2000 else output,
        "config": {
            "mask": mask,
            "ltp_runner": ltp_runner,
            "ltp_include": ltp_include,
            "ltp_libc": ltp_libc,
            "ltp_suites": ltp_suites,
        },
    }


def run_full_test() -> dict:
    """Run the complete test suite (build both arches + run + score + archive)."""
    cmd = "cd /app && python3 scripts/run_full_test.py"
    code, output = _docker_exec_live(cmd, timeout=2400)  # 40 min

    return {
        "ok": code == 0,
        "exit_code": code,
        "output": output[-10000:] if len(output) > 10000 else output,
    }


# ─── GDB Debugging ────────────────────────────────────────────────────────────

def _find_gdb(arch: str) -> str:
    """Find the correct cross-gdb binary inside Docker."""
    if arch == "rv64":
        return GDB_RV
    return GDB_LA


def debug_start(arch: str) -> dict:
    """Start a GDB debugging session: launch QEMU with -s -S (stopped at entry).
    Returns a session_id. Use debug_cmd() to send GDB commands (batch mode).
    Each debug_cmd() call connects fresh to QEMU's gdbserver and runs commands."""
    if arch not in ("rv64", "la64"):
        return {"ok": False, "error": f"Unknown arch: {arch}. Use 'rv64' or 'la64'."}

    session_id = f"debug_{arch}_{int(time.time())}"

    _docker_exec("pkill -f 'qemu-system' 2>/dev/null || true", timeout=5)

    if arch == "rv64":
        qemu_cmd = (
            "cd /app && qemu-system-riscv64 "
            "-machine virt -nographic -smp 1 -m 1024 "
            "-bios default -no-reboot -rtc base=utc "
            "-kernel kernel-rv "
            "-drive file=sdcard-rv.img,if=none,format=raw,id=x0 "
            "-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 "
            "-device virtio-net-device,netdev=net -netdev user,id=net "
            "-s -S "
            "> /tmp/qemu_debug.log 2>&1"
        )
    else:
        qemu_cmd = (
            "cd /app && qemu-system-loongarch64 "
            "-machine virt -nographic -smp 1 -m 1G "
            "-no-reboot -rtc base=utc "
            "-kernel kernel-la "
            "-drive file=sdcard-la.img,if=none,format=raw,id=x0 "
            "-device virtio-blk-pci,drive=x0 "
            "-device virtio-net-pci,netdev=net0 -netdev user,id=net0 "
            "-s -S "
            "> /tmp/qemu_debug.log 2>&1"
        )

    subprocess.Popen(
        ["docker", "exec", "-d", "-w", "/app", CONTAINER, "bash", "-c", qemu_cmd],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )

    time.sleep(2)
    for attempt in range(10):
        code, out, _ = _docker_exec("echo '' | timeout 3 nc -w 1 localhost 1234 2>/dev/null; echo $?", timeout=5)
        if code == 0:
            break
        time.sleep(1)
    else:
        return {"ok": False, "error": "QEMU did not start or GDB port 1234 not ready after 10 attempts."}

    _debug_sessions[session_id] = {
        "arch": arch,
        "started_at": time.time(),
        "qemu_pid": None,
    }
    return {
        "ok": True,
        "session_id": session_id,
        "arch": arch,
        "qemu_status": "running (stopped at entry, -s -S)",
        "hint": "QEMU is stopped at the kernel entry point. Use debug_cmd() to interact. "
                "Common first commands: 'b rust_main' then 'c' to break at rust_main, "
                "or 'bt' + 'info registers' for crash analysis. "
                "Compound commands with semicolons or newlines are supported.",
    }


def debug_cmd(session_id: str, command: str) -> dict:
    """Execute GDB command(s) against the running QEMU gdbserver (batch mode).
    Each call creates a fresh GDB connection. Supports compound commands separated
    by newlines or semicolons.

    Examples:
      - "bt"                               — backtrace
      - "info registers"                   — show all registers
      - "b trap_handler\\nc\\nbt"          — set breakpoint, continue, then backtrace
      - "x/20i $pc"                        — disassemble 20 instructions at PC
      - "p/x $scause"                      — print scause register in hex
      - "info frame"                       — show current stack frame details
    """
    if session_id not in _debug_sessions:
        return {"ok": False, "error": f"Unknown session: {session_id}. Use debug_start() first."}

    session = _debug_sessions[session_id]
    arch = session["arch"]
    gdb_path = _find_gdb(arch)
    kernel_bin = "kernel-rv" if arch == "rv64" else "kernel-la"

    commands = command.replace("\\n", "\n")
    ex_args: list[str] = []
    for cmd in commands.split("\n"):
        cmd = cmd.strip()
        if cmd:
            ex_args.extend(["-ex", cmd])

    full_cmd_parts = [
        f"cd /app && {gdb_path} -q -nx -batch",
        f'-ex "set confirm off"',
        f'-ex "set pagination off"',
        f'-ex "set architecture riscv:rv64"' if arch == "rv64" else "",
        f'-ex "target remote localhost:{QEMU_GDB_PORT}"',
    ]
    for cmd in commands.split("\n"):
        cmd = cmd.strip()
        if cmd:
            full_cmd_parts.append(f'-ex "{cmd}"')

    full_cmd = " ".join(p for p in full_cmd_parts if p) + " 2>&1"

    code, out, err = _docker_exec(full_cmd, timeout=60)

    if "Connection refused" in out or "Remote connection closed" in (err or ""):
        del _debug_sessions[session_id]
        return {"ok": False, "error": "QEMU gdbserver is no longer running. Start a new debug session."}

    return {
        "ok": True,
        "session_id": session_id,
        "command": command,
        "output": out[:8000] if len(out) > 8000 else out,
    }


def debug_stop(session_id: str) -> dict:
    """Stop a GDB debugging session and kill QEMU."""
    if session_id not in _debug_sessions:
        return {"ok": False, "error": f"Unknown session: {session_id}"}

    _debug_sessions.pop(session_id)
    _docker_exec("pkill -f 'qemu-system' 2>/dev/null || true", timeout=5)
    return {"ok": True, "session_id": session_id, "status": "stopped"}


# ─── Status ───────────────────────────────────────────────────────────────────

def kernel_status() -> dict:
    """Check Docker container status, available binaries, and current config."""
    docker_ok = _check_docker()

    result: dict[str, Any] = {
        "docker_running": docker_ok,
    }

    if not docker_ok:
        result["error"] = "Docker container is not running. Start it with: make docker"
        return result

    # Check kernel binaries
    code, out, _ = _docker_exec("ls -la /app/kernel-rv /app/kernel-la 2>/dev/null || echo 'MISSING'")
    result["binaries"] = out

    # Check test config
    code, out, _ = _docker_exec("cat /app/os_test.conf 2>/dev/null | head -20 || echo 'NO_CONFIG'")
    result["test_config"] = out

    # Check debug sessions
    result["active_debug_sessions"] = list(_debug_sessions.keys())

    return result


# ─── MCP Server Setup ─────────────────────────────────────────────────────────

server = Server("kernel-dev")

TOOLS = [
    Tool(
        name="kernel_build",
        description="Compile the OS kernel only (fast, for iteration). Use arch='rv64' or 'la64'. "
                    "Optional: log='off'|'error'|'warn'|'info'|'debug'|'trace'.",
        inputSchema={
            "type": "object",
            "properties": {
                "arch": {"type": "string", "description": "Architecture: 'rv64' or 'la64'"},
                "log": {"type": "string", "description": "Log level (default: 'off')"},
            },
            "required": ["arch"],
        },
    ),
    Tool(
        name="kernel_build_all",
        description="Full build: kernel + user programs + filesystem image. "
                    "Use arch='rv64' or 'la64'. Takes 3-5 minutes.",
        inputSchema={
            "type": "object",
            "properties": {
                "arch": {"type": "string", "description": "Architecture: 'rv64' or 'la64'"},
                "log": {"type": "string", "description": "Log level (default: 'off')"},
            },
            "required": ["arch"],
        },
    ),
    Tool(
        name="kernel_run",
        description="Build and run QEMU with full test image. Returns kernel boot + test output. "
                    "Use log='info' to see syscall traces. Timeout default 300s.",
        inputSchema={
            "type": "object",
            "properties": {
                "arch": {"type": "string", "description": "Architecture: 'rv64' or 'la64'"},
                "log": {"type": "string", "description": "Log level (default: 'off')"},
                "timeout": {"type": "integer", "description": "Timeout in seconds (default: 300)"},
            },
            "required": ["arch"],
        },
    ),
    Tool(
        name="kernel_test_config",
        description="Modify os_test.conf and inject it into the test image. "
                    "mask examples: 0x001=basic, 0x003=basic+busybox, 0xFFF=all. "
                    "ltp_runner: 'script' (production), 'inline' (local debug), or 'suite' (new ltprunner). "
                    "ltp_include: comma-separated test names like 'read01,write01'. "
                    "ltp_libc: 'musl', 'glibc', or 'both'. "
                    "ltp_suites: comma-separated suite names (e.g. 'smoketest,fs'), only used when ltp_runner=suite.",
        inputSchema={
            "type": "object",
            "properties": {
                "arch": {"type": "string", "description": "Architecture: 'rv64' or 'la64'"},
                "mask": {"type": "string", "description": "12-bit test mask hex (e.g. '0x001')"},
                "ltp_runner": {"type": "string", "description": "'script', 'inline', or 'suite'"},
                "ltp_include": {"type": "string", "description": "Comma-separated LTP test names"},
                "ltp_libc": {"type": "string", "description": "'musl', 'glibc', or 'both'"},
                "ltp_suites": {"type": "string", "description": "Comma-separated LTP suite names, for ltp_runner=suite"},
            },
            "required": ["arch"],
        },
    ),
    Tool(
        name="kernel_full_test",
        description="Run the complete test suite: build both arches, run QEMU, score, archive. "
                    "Takes ~40 minutes. Use sparingly.",
        inputSchema={
            "type": "object",
            "properties": {},
        },
    ),
    Tool(
        name="kernel_debug_start",
        description="Start a GDB debugging session. Launches QEMU with -s -S (stops at entry), "
                    "connects cross-gdb. Returns a session_id for kernel_debug_cmd() and kernel_debug_stop().",
        inputSchema={
            "type": "object",
            "properties": {
                "arch": {"type": "string", "description": "Architecture: 'rv64' or 'la64'"},
            },
            "required": ["arch"],
        },
    ),
    Tool(
        name="kernel_debug_cmd",
        description="Send a GDB command to an active debug session. Common commands: "
                    "'b function_name', 'c' (continue), 'bt' (backtrace), 'info registers', "
                    "'x/10x $sp' (examine memory), 'step', 'next', 'p variable'.",
        inputSchema={
            "type": "object",
            "properties": {
                "session_id": {"type": "string", "description": "Session ID from debug_start()"},
                "command": {"type": "string", "description": "GDB command to execute"},
            },
            "required": ["session_id", "command"],
        },
    ),
    Tool(
        name="kernel_debug_stop",
        description="Stop a GDB debug session and kill the associated QEMU process.",
        inputSchema={
            "type": "object",
            "properties": {
                "session_id": {"type": "string", "description": "Session ID from debug_start()"},
            },
            "required": ["session_id"],
        },
    ),
    Tool(
        name="kernel_status",
        description="Check overall status: Docker container, compiled binaries, test config, active debug sessions.",
        inputSchema={"type": "object", "properties": {}},
    ),
]

HANDLERS = {
    "kernel_build": lambda args: build_kernel(args["arch"], args.get("log", "off")),
    "kernel_build_all": lambda args: build_all(args["arch"], args.get("log", "off")),
    "kernel_run": lambda args: run_qemu(args["arch"], args.get("log", "off"), args.get("timeout", 300)),
    "kernel_test_config": lambda args: inject_test_config(
        args["arch"],
        args.get("mask", "0x001"),
        args.get("ltp_runner", "script"),
        args.get("ltp_include", ""),
        args.get("ltp_libc", "both"),
        args.get("ltp_suites", ""),
    ),
    "kernel_full_test": lambda args: run_full_test(),
    "kernel_debug_start": lambda args: debug_start(args["arch"]),
    "kernel_debug_cmd": lambda args: debug_cmd(args["session_id"], args["command"]),
    "kernel_debug_stop": lambda args: debug_stop(args["session_id"]),
    "kernel_status": lambda args: kernel_status(),
}


@server.list_tools()
async def list_tools() -> list[Tool]:
    return TOOLS


@server.call_tool()
async def call_tool(name: str, arguments: dict) -> list[TextContent]:
    handler = HANDLERS.get(name)
    if handler is None:
        return [TextContent(type="text", text=json.dumps({"error": f"Unknown tool: {name}"}))]

    try:
        result = handler(arguments)
        return [TextContent(type="text", text=json.dumps(result, indent=2, ensure_ascii=False))]
    except Exception as e:
        return [TextContent(type="text", text=json.dumps({"ok": False, "error": str(e)}, indent=2))]


async def main():
    async with stdio_server() as (read_stream, write_stream):
        await server.run(
            read_stream,
            write_stream,
            InitializationOptions(
                server_name="kernel-dev",
                server_version="1.0.0",
                capabilities=ServerCapabilities(tools={}),
            ),
        )


if __name__ == "__main__":
    import asyncio
    asyncio.run(main())
