"""BytesIO / memory buffer benchmark.

Stress-tests CPython's io.BytesIO and io.StringIO with large buffers,
many small writes, seek/tell/truncate patterns, and getvalue().
"""
import io


def benchmark():
    """Benchmark: run heavy memory-buffer operations."""

    # BytesIO: many small writes
    buf = io.BytesIO()
    for i in range(200000):
        buf.write(b"data-%d\n" % i)

    # getvalue — slurp whole buffer
    data = buf.getvalue()

    # seek + read pattern
    buf.seek(0)
    chunk1 = buf.read(4096)
    buf.seek(100000)
    chunk2 = buf.read(4096)
    buf.seek(-8192, io.SEEK_END)
    chunk3 = buf.read(4096)

    # truncate
    buf.truncate(50000)
    buf.seek(0)
    truncated = buf.read()

    # StringIO: large text buffer
    sbuf = io.StringIO()
    for i in range(100000):
        sbuf.write(f"line-{i:06d}\n")
    text = sbuf.getvalue()

    # StringIO seek + readline
    sbuf.seek(0)
    lines = []
    for _ in range(5000):
        line = sbuf.readline()
        if not line:
            break
        lines.append(line)

    # Many write/seek cycles (alternating)
    buf2 = io.BytesIO()
    for i in range(50000):
        buf2.write(b"x" * 100)
        if i % 100 == 0:
            buf2.seek(0)
            _ = buf2.read(200)
            buf2.seek(0, io.SEEK_END)

    # Large single write
    large = io.BytesIO()
    large.write(b"A" * (4 * 1024 * 1024))  # 4MB
    _ = large.getvalue()

    # tell consistency
    buf3 = io.BytesIO(b"hello")
    _ = buf3.tell()
    buf3.read()
    _ = buf3.tell()

    return len(data) + len(chunk1) + len(chunk2) + len(chunk3) + len(truncated) + len(text) + len(lines)  # noqa: E501


if __name__ == "__main__":
    benchmark()
