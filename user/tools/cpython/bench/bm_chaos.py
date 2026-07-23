"""Chaos game / strange attractor fractal benchmark.

Adapted from https://github.com/python/performance
Exercises floating-point math, dictionary operations, and nested loops.
Computes a simple "chaos game" — the Henon map attractor.
"""


def henon_map(x, y, a=1.4, b=0.3):
    """Compute one iteration of the Henon map."""
    return 1.0 - a * x * x + y, b * x


def iterate_map(n):
    """Iterate the Henon map n times, recording frequencies in a dict.

    Returns a dict mapping grid cell keys to hit counts.
    Uses dictionary ops extensively.
    """
    x, y = 0.1, 0.0
    hist = {}
    for _ in range(n):
        x, y = henon_map(x, y)
        # Quantize to a 500x500 grid for histogram
        ix = int(x * 250 + 250)
        iy = int(y * 250 + 250)
        if 0 <= ix < 500 and 0 <= iy < 500:
            key = (ix, iy)
            hist[key] = hist.get(key, 0) + 1
    return hist


def benchmark():
    """Benchmark: run 200000 iterations of the Henon map with dict histogram."""
    histogram = iterate_map(500000)
    if not histogram:
        raise RuntimeError("Henon histogram is empty")
    return len(histogram), sum(histogram.values()), max(histogram.values())


if __name__ == "__main__":
    benchmark()
