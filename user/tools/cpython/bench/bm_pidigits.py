"""Compute pi digits using pyperformance's spigot algorithm."""

import itertools


def compose(a, b):
    aq, ar, as_, at = a
    bq, br, bs, bt = b
    return (
        aq * bq,
        aq * br + ar * bt,
        as_ * bq + at * bs,
        as_ * br + at * bt,
    )


def extract(z, j):
    q, r, s, t = z
    return (q * j + r) // (s * j + t)


def pi_digits():
    """Generate digits with the algorithm used by python/pyperformance."""
    z = (1, 0, 0, 1)
    terms = map(lambda k: (k, 4 * k + 2, 0, 2 * k + 1), itertools.count(1))
    while True:
        digit = extract(z, 3)
        while digit != extract(z, 4):
            z = compose(z, next(terms))
            digit = extract(z, 3)
        z = compose((10, -10 * digit, 0, 1), z)
        yield digit


def calc(n):
    """Compute n digits of pi and return them as a string."""
    return "".join(str(d) for d in itertools.islice(pi_digits(), n))


def benchmark():
    """Benchmark: compute 2000 digits of pi."""
    digits = calc(2000)
    if len(digits) != 2000 or not digits.startswith("3141592653"):
        raise RuntimeError("pi digit result mismatch")
    return digits[-16:]


if __name__ == "__main__":
    benchmark()
