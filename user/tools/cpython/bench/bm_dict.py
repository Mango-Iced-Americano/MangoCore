"""Dictionary operations benchmark.

Stress-tests CPython dict: creation, lookup, update, iteration, and
collision-heavy workloads. Exercises millions of get/set/delete ops.
"""


def benchmark():
    """Benchmark: run heavy dictionary operations."""

    # Bulk creation via dict comprehension
    d = {i: i * 2 for i in range(500000)}

    # Lookup — millions of get operations
    for i in range(500000):
        _ = d.get(i, -1)

    # Update — overwrite half the keys
    for i in range(0, 500000, 2):
        d[i] = i * 3

    # setdefault on alternating missing/existing
    for i in range(400000, 600000):
        _ = d.setdefault(i, -i)

    # Iteration — sum all values
    total = sum(v for v in d.values() if v is not None)

    # Collision-heavy: string keys from a small pool
    d2 = {}
    pool = ["key_a", "key_b", "key_c", "key_d", "key_e"]
    for i in range(300000):
        k = pool[i % 5]
        d2[k] = d2.get(k, 0) + 1

    # Pop items one by one
    while len(d2) > 0:
        k, v = d2.popitem()

    # update() / merge many small dicts
    base = {}
    for i in range(5000):
        base.update({f"k{j}": j for j in range(20)})

    # in-operator on large dict
    for i in range(100000, 600000, 7):
        _ = i in d

    # dict from keys
    d3 = dict.fromkeys(range(100000), "default")

    return len(d) + total + len(base) + len(d3)


if __name__ == "__main__":
    benchmark()
