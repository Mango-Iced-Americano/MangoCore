"""Decimal arithmetic benchmark.

Stress-tests CPython's decimal module: creation, arithmetic,
comparison, sqrt, and high-precision operations.
"""
from decimal import Decimal, localcontext


def benchmark():
    """Benchmark: run heavy decimal operations."""

    with localcontext() as context:
        context.prec = 28

        decs = [Decimal(str(i * 0.37)) for i in range(180000)]

        total = Decimal(0)
        for d in decs:
            total += d

        product = Decimal(1)
        for d in decs[:40000]:
            if d != 0:
                product *= d

        quotients = []
        for i, d in enumerate(decs[:40000]):
            if d != 0 and i > 0:
                quotients.append(decs[i - 1] / d)

        half = len(decs) // 2
        gt = sum(1 for a, b in zip(decs[:half], decs[half:]) if a > b)
        lt = sum(1 for a, b in zip(decs[:half], decs[half:]) if a < b)

        roots = [d.sqrt() for d in decs[:30000] if d > 0]

        context.prec = 100
        e = Decimal(0)
        fact = Decimal(1)
        for i in range(1, 3000):
            fact *= i
            e += Decimal(1) / fact
        _ = str(e)

        val = Decimal("3.1415926535897932384626433832795028841971")
        for exp in range(-15, 1):
            _ = val.quantize(Decimal(10) ** exp)

        return len(decs) + len(roots) + gt + lt + len(quotients)


if __name__ == "__main__":
    benchmark()
