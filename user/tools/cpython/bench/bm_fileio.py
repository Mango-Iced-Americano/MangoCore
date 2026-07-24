"""File-system and block-I/O workload with explicit phase timings.

The board wrapper pins ``CPYTHON_BENCH_STORAGE_DIR`` to the selected writable
test filesystem.  Production defaults preserve the full workload; focused
diagnostic runs may reduce operation counts through the documented environment
variables without changing those defaults.
"""

import os
import tempfile
import time


_CHUNK = b"X" * 4096
_SHORT_PROFILE = os.environ.get("CPYTHON_FILEIO_PROFILE") == "diag-short"
_CHUNK_COUNT = int(
    os.environ.get("CPYTHON_FILEIO_CHUNK_COUNT", "64" if _SHORT_PROFILE else "2560")
)
_SMALL_FILE_COUNT = int(
    os.environ.get("CPYTHON_FILEIO_SMALL_FILE_COUNT", "100" if _SHORT_PROFILE else "5000")
)
_DIRECT_COUNT = int(
    os.environ.get("CPYTHON_FILEIO_DIRECT_COUNT", "4" if _SHORT_PROFILE else "50")
)


def _elapsed(started_ns):
    return time.perf_counter_ns() - started_ns


def benchmark():
    storage_root = os.environ.get("CPYTHON_BENCH_STORAGE_DIR") or os.environ.get("TMPDIR")
    if storage_root:
        os.makedirs(storage_root, exist_ok=True)

    metrics = {}
    with tempfile.TemporaryDirectory(prefix="mango-fileio-", dir=storage_root) as tmpdir:
        fpath = os.path.join(tmpdir, "sequential.bin")
        started = time.perf_counter_ns()
        with open(fpath, "wb", buffering=0) as output:
            for _ in range(_CHUNK_COUNT):
                written = output.write(_CHUNK)
                if written != len(_CHUNK):
                    raise RuntimeError("short sequential write")
            os.fsync(output.fileno())
        metrics["sequential_write_fsync_ns"] = _elapsed(started)

        started = time.perf_counter_ns()
        total_read = 0
        with open(fpath, "rb", buffering=0) as source:
            while True:
                data = source.read(65536)
                if not data:
                    break
                total_read += len(data)
        if total_read != len(_CHUNK) * _CHUNK_COUNT:
            raise RuntimeError("short hot-cache sequential read")
        metrics["sequential_hot_read_ns"] = _elapsed(started)

        started = time.perf_counter_ns()
        for index in range(_SMALL_FILE_COUNT):
            path = os.path.join(tmpdir, "small_%d.txt" % index)
            expected = ("data-%06d\n" % index) * 5
            with open(path, "w", encoding="utf-8") as output:
                output.write(expected)
            with open(path, "r", encoding="utf-8") as source:
                if source.read() != expected:
                    raise RuntimeError("small-file content mismatch")
            os.remove(path)
        metrics["metadata_%d_files_ns" % _SMALL_FILE_COUNT] = _elapsed(started)

        direct_path = os.path.join(tmpdir, "direct.bin")
        started = time.perf_counter_ns()
        descriptor = os.open(direct_path, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o644)
        try:
            payload = b"0" * 65536
            for _ in range(_DIRECT_COUNT):
                if os.write(descriptor, payload) != len(payload):
                    raise RuntimeError("short direct write")
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        metrics["direct_write_fsync_ns"] = _elapsed(started)

        started = time.perf_counter_ns()
        descriptor = os.open(direct_path, os.O_RDONLY)
        try:
            direct_read = 0
            while True:
                data = os.read(descriptor, 65536)
                if not data:
                    break
                direct_read += len(data)
        finally:
            os.close(descriptor)
        if direct_read != _DIRECT_COUNT * 65536:
            raise RuntimeError("short direct read")
        metrics["direct_hot_read_ns"] = _elapsed(started)

        started = time.perf_counter_ns()
        seek_path = os.path.join(tmpdir, "seek.bin")
        with open(seek_path, "wb+") as stream:
            stream.write(b"A" * 200000)
            stream.seek(0)
            if len(stream.read(500)) != 500:
                raise RuntimeError("short seek read at start")
            stream.seek(50000)
            if len(stream.read(500)) != 500:
                raise RuntimeError("short seek read in middle")
            stream.truncate(50000)
            stream.flush()
            os.fsync(stream.fileno())
        if os.stat(seek_path).st_size != 50000:
            raise RuntimeError("truncate size mismatch")
        metrics["seek_truncate_fsync_ns"] = _elapsed(started)

    metrics["bytes_sequential"] = len(_CHUNK) * _CHUNK_COUNT
    metrics["small_file_count"] = _SMALL_FILE_COUNT
    return metrics


if __name__ == "__main__":
    benchmark()
