"""String operations benchmark.

Stress-tests CPython string internals: join, split, replace, format,
and find on large text bodies. Exercises memory allocation for string ops.
"""
import re

# Generate a 200KB text block with variety
_BASE = ("The quick brown fox jumps over 123 lazy dogs. " * 30 + "\n")
_TEXT = _BASE * 1200  # ~200KB

# Large list of strings for join/split
_NUMS = [str(i) for i in range(80000)]


def benchmark():
    """Benchmark: run heavy string operations."""

    # join — large string concatenation
    s1 = " ".join(_NUMS)

    # split — reverse of join
    parts = s1.split(" ")

    # replace — many replacements on large text
    s2 = _TEXT.replace("fox", "F0X").replace("lazy", "LAZY")

    # find/rfind — scan for substrings
    for needle in ("fox", "jumps", "123", "THE", "zzz"):
        _ = _TEXT.find(needle)
        _ = _TEXT.rfind(needle)

    # f-string formatting in a loop
    accum = []
    for i in range(2000):
        accum.append(f"item_{i:06d}_value={i * 1.5:.3f}")
    _ = "|".join(accum)

    # regex substitution on large text
    pat = re.compile(r"fox|dog|lazy")
    s3 = pat.sub("___", _TEXT)

    # count character occurrences
    for ch in "aeiouAEIOU":
        _ = _TEXT.count(ch)

    # startswith / endswith
    _ = _TEXT.startswith("The")
    _ = _TEXT.endswith("\n")

    # Prevent dead-code elimination
    return len(s1) + len(parts) + len(s2) + len(s3)


if __name__ == "__main__":
    benchmark()
