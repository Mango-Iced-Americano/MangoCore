"""List operations benchmark.

Stress-tests CPython list: comprehension, slicing, sorting, appending,
extending, popping, and copying large lists.
"""
import random


def benchmark():
    """Benchmark: run heavy list operations."""

    # Bulk creation
    lst = list(range(2_000_000))

    # List comprehension with condition
    evens = [x for x in lst if x % 2 == 0]

    # Slice copy
    half = lst[:1_000_000]

    # Deep copy via slice
    copy1 = lst[:]

    # Sorting random data (smaller to keep total time down)
    rng = random.Random(42)
    rand_lst = [rng.random() for _ in range(100000)]
    rand_lst.sort()

    # Sorting with key function
    pairs = [(rng.random(), rng.randint(0, 10000)) for _ in range(50000)]
    pairs.sort(key=lambda p: p[1])
    pairs.sort(key=lambda p: p[0])

    # Already-sorted sort (best-case Timsort)
    sorted_lst = list(range(50000))
    sorted_lst.sort()

    # Reverse-sorted sort (tricky for Timsort)
    rev_lst = list(range(50000, 0, -1))
    rev_lst.sort()

    # extend
    ext = []
    for i in range(100):
        ext.extend(range(20000))

    # append + pop (stack pattern)
    stack = []
    for i in range(100000):
        stack.append(i)
    while stack:
        stack.pop()

    # map vs comprehension
    _ = list(map(lambda x: x * 2, range(500000)))

    # reversed iteration + sum
    total = sum(x for x in reversed(copy1) if x % 3 == 0)

    # count / index
    _ = lst.count(42)
    try:
        _ = lst.index(1_999_999)
    except ValueError:
        pass

    return len(evens) + len(half) + total + len(ext)


if __name__ == "__main__":
    benchmark()
