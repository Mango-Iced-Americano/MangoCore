import sys
import time
import queue
import threading
from contextlib import contextmanager

fail = 0
PREFIX = "[CPYTHON L8]"


@contextmanager
def check(name):
    global fail
    print(f"{PREFIX} test: {name}", flush=True)
    try:
        yield
        print(f"{PREFIX} test: {name} PASS", flush=True)
    except Exception as e:
        print(f"{PREFIX} test: {name} FAIL ({type(e).__name__}: {e})", flush=True)
        fail = 1


with check("thread start join"):
    result = []

    def worker():
        result.append("ran")

    t = threading.Thread(target=worker)
    t.start()
    t.join(timeout=5.0)
    assert not t.is_alive()
    assert result == ["ran"], result


with check("contended lock acquire release"):
    lock = threading.Lock()
    events = []

    lock.acquire()

    def worker():
        events.append("waiting")
        with lock:
            events.append("acquired")

    t = threading.Thread(target=worker)
    t.start()

    deadline = time.time() + 5.0
    while "waiting" not in events and time.time() < deadline:
        time.sleep(0.01)

    assert "waiting" in events, events
    assert "acquired" not in events, events

    lock.release()
    t.join(timeout=5.0)

    assert not t.is_alive()
    assert events == ["waiting", "acquired"], events


with check("queue put get"):
    q = queue.Queue()
    out = []

    def producer():
        q.put("hello")
        q.put("world")

    def consumer():
        out.append(q.get(timeout=5.0))
        out.append(q.get(timeout=5.0))

    tp = threading.Thread(target=producer)
    tc = threading.Thread(target=consumer)
    tc.start()
    tp.start()
    tp.join(timeout=5.0)
    tc.join(timeout=5.0)

    assert not tp.is_alive()
    assert not tc.is_alive()
    assert out == ["hello", "world"], out


with check("four threads increment counter"):
    lock = threading.Lock()
    counter = {"value": 0}

    def increment():
        for _ in range(25):
            with lock:
                counter["value"] += 1

    threads = [threading.Thread(target=increment) for _ in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=5.0)

    assert all(not t.is_alive() for t in threads)
    assert counter["value"] == 100, counter["value"]


with check("daemon thread starts"):
    started = threading.Event()
    stop = threading.Event()

    def daemon_worker():
        started.set()
        while not stop.is_set():
            time.sleep(0.01)

    t = threading.Thread(target=daemon_worker, daemon=True)
    t.start()

    assert t.daemon
    assert started.wait(timeout=5.0)

    stop.set()
    t.join(timeout=5.0)
    assert not t.is_alive()


print(f"{PREFIX} RESULT {'PASS' if fail == 0 else 'FAIL'}", flush=True)
sys.exit(fail)
