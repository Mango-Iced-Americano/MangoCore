"""Float operations benchmark — heavy use of math module.

Adapted from https://github.com/python/performance
Exercises float addition, multiplication, division, sqrt, sin, cos,
log, and pow operations in a tight loop.
"""
import math


def float_ops(n):
    """Perform n iterations of mixed float operations."""
    x = 0.0
    for i in range(1, n + 1):
        a = i * 0.0001
        b = a * a + 1.0
        x += math.sqrt(b) * 0.5
        x += math.sin(a) * 0.3
        x += math.cos(a * 0.7) * 0.2
        x += math.log(1.0 + b) * 0.1
        x += math.pow(b, 0.25) * 0.05
        x += math.exp(math.sin(a) * 0.01) * 0.02
    return x


def benchmark():
    """Benchmark: run mixed float operations."""
    result = float_ops(600000)
    if not math.isfinite(result) or result <= 0.0:
        raise RuntimeError("float result is invalid")
    return round(result, 8)


if __name__ == "__main__":
    benchmark()
