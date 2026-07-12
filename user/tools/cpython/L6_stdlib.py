"""L6: CPython standard library core module smoke test."""
import sys
from contextlib import contextmanager

fail = 0

@contextmanager
def check(name):
    global fail
    print(f"[CPYTHON L6] test: {name}", flush=True)
    try:
        yield
        print(f"[CPYTHON L6] test: {name} PASS", flush=True)
    except Exception as e:
        print(f"[CPYTHON L6] test: {name} FAIL ({e})", flush=True)
        fail = 1

def test_stdlib():
    # os — filesystem / process info
    with check("os.getcwd"):
        import os
        cwd = os.getcwd()
        assert len(cwd) > 0

    with check("os.environ"):
        import os
        home = os.environ.get("HOME", "")
        assert len(home) > 0

    with check("os.listdir"):
        import os
        entries = os.listdir("/")
        assert len(entries) > 0

    # time — clock syscalls
    with check("time.time"):
        import time
        t = time.time()
        assert t > 0

    with check("time.monotonic"):
        import time
        t = time.monotonic()
        assert t > 0

    # math — basic FP
    with check("math"):
        import math
        assert abs(math.sqrt(4.0) - 2.0) < 0.0001
        assert abs(math.sin(0.0)) < 0.0001
        assert math.pi > 3.0

    # json — pure Python serialization
    with check("json"):
        import json
        data = {"key": [1, 2, 3]}
        encoded = json.dumps(data)
        decoded = json.loads(encoded)
        assert decoded == data

    # re — regex engine
    with check("re.match"):
        import re
        m = re.match(r"hello (\w+)", "hello world")
        assert m is not None
        assert m.group(1) == "world"

    # random — getrandom / /dev/urandom
    with check("random.randint"):
        import random
        r = random.randint(1, 1000)
        assert 1 <= r <= 1000

    # hashlib — crypto hash
    with check("hashlib.sha256"):
        import hashlib
        h = hashlib.sha256(b"MangoCore")
        digest = h.hexdigest()
        assert len(digest) == 64
        assert digest != hashlib.sha256(b"different").hexdigest()

    # zlib — compression
    with check("zlib"):
        import zlib
        data = b"MangoCore " * 100
        compressed = zlib.compress(data)
        assert len(compressed) < len(data)
        decompressed = zlib.decompress(compressed)
        assert decompressed == data

    # select — poll/epoll
    with check("select.select"):
        import select
        r, w, x = select.select([], [], [], 0)
        assert r == [] and w == [] and x == []

    # tempfile — tmpfs write
    with check("tempfile"):
        import tempfile
        f = tempfile.NamedTemporaryFile(delete=False)
        f.write(b"test data\n")
        f.close()
        import os
        os.unlink(f.name)

    # pathlib — filesystem metadata
    with check("pathlib"):
        from pathlib import Path
        p = Path("/")
        assert p.is_dir()
        assert p.exists()

    # struct — binary packing (pure)
    with check("struct"):
        import struct
        packed = struct.pack(">I", 0xDEADBEEF)
        assert len(packed) == 4

    # sqlite3 — extension module loading
    with check("sqlite3"):
        import sqlite3
        import os
        conn = sqlite3.connect(":memory:")
        conn.execute("CREATE TABLE t (x INTEGER)")
        conn.execute("INSERT INTO t VALUES (42)")
        row = conn.execute("SELECT x FROM t").fetchone()
        assert row[0] == 42
        conn.close()

test_stdlib()

if fail == 0:
    print("[CPYTHON L6] stdlib core OK", flush=True)
else:
    print("[CPYTHON L6] stdlib core FAIL", flush=True)

sys.exit(fail)
