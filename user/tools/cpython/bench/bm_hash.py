"""Hash computation benchmark.

Stress-tests CPython's hashlib: SHA-256, SHA-512, MD5, SHA-1.
Exercises large-data hashing, many small hashes, and incremental updates.
"""
import hashlib
# Deterministic input keeps the workload identical across architectures and
# leaves getrandom() measurement to the dedicated kernel/random tests.
_PATTERN = bytes(range(256))
_BIG_DATA = _PATTERN * ((5 * 1024 * 1024) // len(_PATTERN))
_SMALL_DATA = _PATTERN * ((64 * 1024) // len(_PATTERN))


def benchmark():
    """Benchmark: run heavy hashing operations."""

    # SHA-256: large one-shot hash
    h = hashlib.sha256(_BIG_DATA)
    _ = h.hexdigest()

    # SHA-512: large one-shot hash
    h = hashlib.sha512(_BIG_DATA)
    _ = h.hexdigest()

    # MD5: large one-shot hash
    h = hashlib.md5(_BIG_DATA)
    _ = h.hexdigest()

    # SHA-1: large one-shot hash
    h = hashlib.sha1(_BIG_DATA)
    _ = h.hexdigest()

    # SHA-256: incremental (1KB chunks) on big data
    h = hashlib.sha256()
    for i in range(0, len(_BIG_DATA), 1024):
        h.update(_BIG_DATA[i:i + 1024])
    _ = h.digest()

    # SHA-256: many small hashes
    for i in range(20000):
        h = hashlib.sha256()
        h.update(b"hello-%d-world" % i)
        _ = h.digest()

    # SHA-256: large update chunks
    h = hashlib.sha256()
    chunk = 1024 * 1024  # 1MB
    for i in range(0, len(_BIG_DATA), chunk):
        h.update(_BIG_DATA[i:i + chunk])
    _ = h.hexdigest()

    # hashlib.new() dynamic construction
    for name in ("sha256", "sha512", "md5", "sha1", "sha3_256", "blake2b"):
        try:
            h = hashlib.new(name, _SMALL_DATA)
            _ = h.hexdigest()
        except (ValueError, TypeError):
            pass

    # digest() for various algorithms
    for algo in (hashlib.sha256, hashlib.sha1, hashlib.md5):
        h = algo(_SMALL_DATA)
        _ = h.digest()
        _ = h.digest_size
        _ = h.block_size

    # copy() — clone and continue
    h1 = hashlib.sha256(b"prefix-")
    h2 = h1.copy()
    h2.update(b"suffix")
    result = h2.hexdigest()
    if len(result) != 64:
        raise RuntimeError("unexpected SHA-256 digest length")
    return result


if __name__ == "__main__":
    benchmark()
