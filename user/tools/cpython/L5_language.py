"""L5: CPython core language features smoke test."""
import sys
from contextlib import contextmanager

fail = 0

@contextmanager
def check(name):
    global fail
    print(f"[CPYTHON L5] test: {name}", flush=True)
    try:
        yield
        print(f"[CPYTHON L5] test: {name} PASS", flush=True)
    except Exception as e:
        print(f"[CPYTHON L5] test: {name} FAIL ({e})", flush=True)
        fail = 1

def test_basic():
    # Arithmetic
    with check("arithmetic"):
        assert 1 + 1 == 2
        assert 3 * 4 == 12
        assert 10 // 3 == 3
        assert 10 % 3 == 1
        assert 2 ** 10 == 1024
        assert -(-5) == 5

    # Strings and bytes
    with check("strings"):
        assert len("hello") == 5
        assert "hello" + " world" == "hello world"
        assert "hello"[1:4] == "ell"
        assert b"abc".decode() == "abc"
        assert "abc".encode() == b"abc"

    # Lists
    with check("lists"):
        lst = [1, 2, 3]
        lst.append(4)
        assert lst == [1, 2, 3, 4]
        assert lst.pop() == 4
        assert len(lst) == 3

    # Dicts
    with check("dicts"):
        d = {"a": 1, "b": 2}
        assert d["a"] == 1
        assert d.get("c", 99) == 99
        assert set(d.keys()) == {"a", "b"}

    # List comprehensions
    with check("comprehensions"):
        assert [x * x for x in range(5)] == [0, 1, 4, 9, 16]
        assert {x: x * 2 for x in range(3)} == {0: 0, 1: 2, 2: 4}

    # Exceptions
    with check("exceptions"):
        caught = False
        try:
            raise ValueError("test error")
        except ValueError:
            caught = True
        assert caught

    # Functions and closures
    with check("functions"):
        def adder(n):
            return lambda x: x + n
        f = adder(10)
        assert f(5) == 15

    # Loops
    with check("loops"):
        total = 0
        for i in range(1, 11):
            total += i
        assert total == 55

    # Booleans and None
    with check("booleans"):
        assert True and True
        assert not False
        assert None is None
        assert 0 != None

    # Set operations
    with check("sets"):
        a = {1, 2, 3}
        b = {3, 4, 5}
        assert a | b == {1, 2, 3, 4, 5}
        assert a & b == {3}

test_basic()

if fail == 0:
    print("[CPYTHON L5] language core OK", flush=True)
else:
    print("[CPYTHON L5] language core FAIL", flush=True)

sys.exit(fail)
