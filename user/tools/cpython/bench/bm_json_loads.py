"""JSON deserialization benchmark.

Adapted from https://github.com/python/performance
Benchmarks json.loads() on moderately sized structured data.
"""
import json
import random


def _build_data():
    """Generate a structured JSON blob with various data types."""
    rng = random.Random(0x4D414E47)
    data = {
        "version": 2,
        "metadata": {
            "title": "Sample Benchmark Data",
            "author": "python/performance",
            "timestamp": 1234567890.123,
            "tags": ["benchmark", "json", "python"],
            "valid": True,
            "count": 42,
        },
        "records": [],
    }
    for i in range(200):
        record = {
            "id": i,
            "name": f"item-{i}",
            "values": [rng.random() for _ in range(10)],
            "flags": [True, False, True, False],
            "nested": {
                "x": i * 1.5,
                "y": i * 2.5,
                "label": f"point_{i}",
            },
        }
        data["records"].append(record)
    return data


DATA = _build_data()
SERIALIZED = json.dumps(DATA)


def benchmark():
    """Benchmark: deserialize a JSON string ~5KB in size."""
    obj = None
    for _ in range(2500):
        obj = json.loads(SERIALIZED)
        # Verify structure to prevent optimization
        _ = len(obj["records"])
    if obj is None or len(obj["records"]) != 200:
        raise RuntimeError("JSON record count mismatch")
    return obj["version"], len(obj["records"]), obj["records"][-1]["id"]


if __name__ == "__main__":
    benchmark()
