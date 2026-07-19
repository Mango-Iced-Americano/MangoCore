"""Spectral norm computation benchmark.

Adapted from https://github.com/python/performance
Originally from the Computer Language Benchmarks Game.
Computes the spectral norm of a matrix using the power method.
Exercises floating-point arithmetic and list comprehensions.
"""
import math


def eval_A(i, j):
    """Compute matrix element A[i][j]."""
    return 1.0 / ((i + j) * (i + j + 1) // 2 + i + 1)


def eval_A_times_u(u):
    """Compute v = A @ u."""
    n = len(u)
    v = [0.0] * n
    for i in range(n):
        total = 0.0
        for j in range(n):
            total += eval_A(i, j) * u[j]
        v[i] = total
    return v


def eval_At_times_u(u):
    """Compute v = A^T @ u."""
    n = len(u)
    v = [0.0] * n
    for i in range(n):
        total = 0.0
        for j in range(n):
            total += eval_A(j, i) * u[j]
        v[i] = total
    return v


def eval_AtA_times_u(u):
    """Compute v = A^T @ A @ u."""
    return eval_At_times_u(eval_A_times_u(u))


def spectral_norm(n):
    """Compute the spectral norm of the n x n matrix A."""
    u = [1.0] * n
    v = [0.0] * n

    for _ in range(10):
        v = eval_AtA_times_u(u)
        u = eval_AtA_times_u(v)

    vbv = sum(v[i] * u[i] for i in range(n))
    vv = sum(v[i] * v[i] for i in range(n))
    return math.sqrt(vbv / vv)


def benchmark():
    """Benchmark: compute spectral norm of a 200x200 matrix."""
    result = spectral_norm(200)
    if not 1.27 < result < 1.28:
        raise RuntimeError("spectral norm result mismatch")
    return round(result, 12)


if __name__ == "__main__":
    benchmark()
