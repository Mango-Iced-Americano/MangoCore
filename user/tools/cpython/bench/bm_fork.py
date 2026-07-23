"""Fork/exec/pipe/wait workload adapted for MangoCore's private musl loader."""

import os
import subprocess
import sys
import time


CHILD_TIMEOUT = float(os.environ.get("CPYTHON_BENCH_CHILD_TIMEOUT", "30"))


def python_command(*arguments):
    python = os.environ.get("CPYTHON_PY") or sys.executable
    loader = os.environ.get("CPYTHON_LD")
    command = []
    if loader:
        command.append(loader)
        library_path = os.environ.get("LD_LIBRARY_PATH")
        if library_path:
            command.extend(("--library-path", library_path))
    command.append(python)
    command.extend(arguments)
    return command


def checked_run(*arguments, **kwargs):
    kwargs.setdefault("timeout", CHILD_TIMEOUT)
    completed = subprocess.run(python_command(*arguments), **kwargs)
    if completed.returncode != 0:
        raise RuntimeError("child failed with status %d" % completed.returncode)
    return completed


def benchmark():
    metrics = {}

    started = time.perf_counter_ns()
    for _ in range(40):
        child = checked_run("-c", "print('hello')", capture_output=True, text=True)
        if child.stdout != "hello\n" or child.stderr:
            raise RuntimeError("spawn output mismatch")
    metrics["spawn_40_ns"] = time.perf_counter_ns() - started

    payload = "x" * 10000
    started = time.perf_counter_ns()
    for _ in range(20):
        child = checked_run(
            "-c",
            "import sys; sys.stdout.write(sys.stdin.read())",
            input=payload,
            capture_output=True,
            text=True,
        )
        if child.stdout != payload:
            raise RuntimeError("pipe echo mismatch")
    metrics["pipe_roundtrip_20_ns"] = time.perf_counter_ns() - started

    started = time.perf_counter_ns()
    for expected in (0, 42, 255):
        child = subprocess.run(
            python_command("-c", "import sys; sys.exit(%d)" % expected),
            capture_output=True,
            timeout=CHILD_TIMEOUT,
        )
        if child.returncode != expected:
            raise RuntimeError("exit status mismatch")
    child = checked_run(
        "-c",
        "import sys; sys.stderr.write('err\\n')",
        capture_output=True,
    )
    if child.stderr != b"err\n":
        raise RuntimeError("stderr capture mismatch")
    child = checked_run(
        "-c",
        "import os; print(os.environ.get('PATH', 'none'))",
        capture_output=True,
        text=True,
    )
    if not child.stdout.strip():
        raise RuntimeError("environment capture mismatch")
    metrics["wait_status_stderr_env_ns"] = time.perf_counter_ns() - started
    metrics["child_process_count"] = 65
    return metrics


if __name__ == "__main__":
    benchmark()
