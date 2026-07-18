import os
import sys
import subprocess
from contextlib import contextmanager

fail = 0
PREFIX = "[CPYTHON L8-SUBPROC]"
RUNTIME_ROOT = os.environ["CPYTHON_ROOT"]


class SkipTest(Exception):
    pass


@contextmanager
def check(name):
    global fail
    print(f"{PREFIX} test: {name}", flush=True)
    try:
        yield
        print(f"{PREFIX} test: {name} PASS", flush=True)
    except SkipTest as e:
        print(f"{PREFIX} test: {name} SKIP ({e})", flush=True)
    except Exception as e:
        print(f"{PREFIX} test: {name} FAIL ({type(e).__name__}: {e})", flush=True)
        fail = 1


def existing_file(paths):
    for path in paths:
        if path and os.path.exists(path):
            return path
    return None


def list_ld_musl_candidates():
    candidates = []

    env_loader = os.environ.get("CPYTHON_MUSL_LOADER") or os.environ.get("MUSL_LOADER")
    if env_loader:
        candidates.append(env_loader)

    dirs = [
        os.path.join(RUNTIME_ROOT, "lib"),
        os.path.join(RUNTIME_ROOT, "usr", "lib"),
        "/lib",
        "/usr/lib",
    ]

    ld_library_path = os.environ.get("LD_LIBRARY_PATH", "")
    for entry in ld_library_path.split(":"):
        if entry:
            dirs.append(entry)

    pyhome = os.environ.get("PYTHONHOME")
    if pyhome:
        dirs.extend([
            os.path.join(pyhome, "lib"),
            os.path.dirname(os.path.dirname(pyhome)),
            os.path.join(os.path.dirname(os.path.dirname(pyhome)), "lib"),
        ])

    for directory in dirs:
        try:
            for name in os.listdir(directory):
                if name.startswith("ld-musl-") and name.endswith(".so.1"):
                    candidates.append(os.path.join(directory, name))
        except Exception:
            pass

    seen = set()
    out = []
    for path in candidates:
        if path not in seen:
            seen.add(path)
            out.append(path)
    return out


def find_python_binary():
    pyhome = os.environ.get("PYTHONHOME")

    candidates = [
        os.environ.get("CPYTHON_PYTHON"),
        sys.executable if os.path.basename(sys.executable).startswith("python") else None,
        os.path.join(pyhome, "bin", "python3") if pyhome else None,
        os.path.join(RUNTIME_ROOT, "usr", "local", "bin", "python3"),
        os.path.join(RUNTIME_ROOT, "usr", "bin", "python3"),
        os.path.join(RUNTIME_ROOT, "bin", "python3"),
        "/usr/local/bin/python3",
        "/usr/bin/python3",
    ]

    python = existing_file(candidates)
    if not python:
        raise RuntimeError("cannot locate python3 binary for subprocess")
    return python


def find_musl_loader():
    return existing_file(list_ld_musl_candidates())


def python_cmd(*args):
    python = find_python_binary()
    loader = find_musl_loader()

    if loader:
        cmd = [loader]
        libpath = os.environ.get("LD_LIBRARY_PATH")
        if libpath:
            cmd.extend(["--library-path", libpath])
        cmd.append(python)
    else:
        cmd = [python]

    cmd.extend(args)
    return cmd


def busybox_cmd(*args):
    if not os.path.exists("/bin/busybox"):
        raise SkipTest("/bin/busybox not present")
    return ["/bin/busybox", *args]


with check("helper locates python subprocess command"):
    cmd = python_cmd("-c", "print(123)")
    assert len(cmd) >= 3 if "ld-musl-" in os.path.basename(cmd[0]) else len(cmd) >= 2
    print(f"{PREFIX} python command: {' '.join(cmd[:4])} ...", flush=True)


with check("subprocess.run capture_output python"):
    cp = subprocess.run(
        python_cmd(
            "-c",
            "import sys; print('child-out'); print('child-err', file=sys.stderr)",
        ),
        capture_output=True,
        text=True,
        timeout=15.0,
    )
    assert cp.returncode == 0, cp.returncode
    assert cp.stdout.strip() == "child-out", repr(cp.stdout)
    assert cp.stderr.strip() == "child-err", repr(cp.stderr)


with check("subprocess.Popen pipe communication"):
    code = (
        "import sys; "
        "data = sys.stdin.read(); "
        "sys.stdout.write(data.upper()); "
        "sys.stdout.flush()"
    )
    p = subprocess.Popen(
        python_cmd("-c", code),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    out, err = p.communicate("hello subprocess\n", timeout=15.0)

    assert p.returncode == 0, p.returncode
    assert out == "HELLO SUBPROCESS\n", repr(out)
    assert err == "", repr(err)


with check("exit code propagation"):
    cp = subprocess.run(
        python_cmd("-c", "import sys; sys.exit(23)"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=15.0,
    )
    assert cp.returncode == 23, cp.returncode


with check("stderr capture"):
    cp = subprocess.run(
        python_cmd("-c", "import sys; sys.stderr.write('stderr-ok\\n')"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=15.0,
    )
    assert cp.returncode == 0, cp.returncode
    assert cp.stdout == "", repr(cp.stdout)
    assert cp.stderr == "stderr-ok\n", repr(cp.stderr)


with check("busybox echo subprocess"):
    cp = subprocess.run(
        busybox_cmd("echo", "hello-busybox"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=10.0,
    )
    assert cp.returncode == 0, cp.returncode
    assert cp.stdout.strip() == "hello-busybox", repr(cp.stdout)


print(f"{PREFIX} RESULT {'PASS' if fail == 0 else 'FAIL'}", flush=True)
sys.exit(fail)
