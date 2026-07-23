"""Thread creation, scheduler, lock and Queue workload with phase timings."""

import queue
import threading
import time


def _noop():
    return None


def _busy_work(count):
    return sum(index * index for index in range(count))


def _join_all(threads):
    for thread in threads:
        thread.join()
        if thread.is_alive():
            raise RuntimeError("thread did not terminate")


def benchmark():
    metrics = {}

    started = time.perf_counter_ns()
    threads = []
    for _ in range(200):
        thread = threading.Thread(target=_noop)
        thread.start()
        threads.append(thread)
    _join_all(threads)
    metrics["thread_create_join_200_ns"] = time.perf_counter_ns() - started

    started = time.perf_counter_ns()
    threads = []
    for _ in range(100):
        thread = threading.Thread(target=_busy_work, args=(5000,))
        thread.start()
        threads.append(thread)
    _join_all(threads)
    metrics["thread_work_join_100_ns"] = time.perf_counter_ns() - started

    started = time.perf_counter_ns()
    lock = threading.Lock()
    for _ in range(500000):
        lock.acquire()
        lock.release()
    lock2 = threading.Lock()
    for _ in range(50000):
        with lock2:
            pass
    rlock = threading.RLock()
    for _ in range(100000):
        with rlock:
            pass
    metrics["uncontended_locks_ns"] = time.perf_counter_ns() - started

    started = time.perf_counter_ns()
    messages = queue.Queue()
    for index in range(100000):
        messages.put(index)
    consumed = 0
    while not messages.empty():
        messages.get()
        consumed += 1
    if consumed != 100000:
        raise RuntimeError("Queue item count mismatch")
    metrics["queue_100000_ns"] = time.perf_counter_ns() - started

    started = time.perf_counter_ns()
    event = threading.Event()
    for _ in range(50000):
        event.set()
        if not event.is_set():
            raise RuntimeError("Event state mismatch")
        event.clear()
    condition = threading.Condition()
    for _ in range(10000):
        with condition:
            condition.notify()
    metrics["event_condition_ns"] = time.perf_counter_ns() - started

    started = time.perf_counter_ns()
    daemon_threads = []
    for _ in range(50):
        thread = threading.Thread(target=_noop, daemon=True)
        thread.start()
        daemon_threads.append(thread)
    _join_all(daemon_threads)
    metrics["daemon_create_join_50_ns"] = time.perf_counter_ns() - started
    metrics["thread_count"] = 350
    return metrics


if __name__ == "__main__":
    benchmark()
