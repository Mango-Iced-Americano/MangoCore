"""Sorting benchmark — exercise Timsort on diverse data.

Stress-tests CPython's list.sort() and sorted() across:
  - random ints, floats, strings, tuples
  - already-sorted, reverse-sorted, partially-sorted
  - custom key functions
  - stable sort property
"""
import random


def benchmark():
    """Benchmark: run heavy sorting operations."""

    rng = random.Random(12345)
    n = 80000

    # Random ints
    ints = [rng.randint(0, n * 10) for _ in range(n)]
    ints.sort()

    # Random floats
    floats = [rng.random() for _ in range(n)]
    floats.sort()

    # Random strings (fixed-length for fair comparison)
    strings = [f"{i:08d}" for i in ints]
    strings.sort()

    # Sorting with key=abs on negatives
    negs = [rng.randint(-10000, 10000) for _ in range(n)]
    negs.sort(key=abs)

    # Sorting tuples (lexicographic)
    tuples = [(rng.randint(0, 1000), rng.randint(0, 1000)) for _ in range(n // 2)]
    tuples.sort()

    # Already-sorted data (Timsort best case)
    sorted_data = list(range(n))
    sorted_data.sort()

    # Reverse-sorted data
    rev_data = list(range(n, 0, -1))
    rev_data.sort()

    # Partially-sorted (every 10th element out of place)
    partial = list(range(n))
    for i in range(0, n, 10):
        partial[i] = rng.randint(0, n)
    partial.sort()

    # sorted() builtin with key
    raw = [(rng.random(), rng.randint(0, 500)) for _ in range(n // 2)]
    result = sorted(raw, key=lambda x: x[1])

    # sort with reverse=True
    rev_floats = [rng.random() for _ in range(n // 2)]
    rev_floats.sort(reverse=True)

    # stability check: sort by secondary then primary key
    items = [(rng.randint(0, 50), rng.randint(0, 1000)) for _ in range(50000)]
    items.sort(key=lambda x: x[1])  # secondary
    items.sort(key=lambda x: x[0])  # primary — must preserve secondary order

    return len(ints) + len(floats) + len(strings) + len(result)


if __name__ == "__main__":
    benchmark()
