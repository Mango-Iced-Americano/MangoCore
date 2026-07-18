#!/usr/bin/env python3
"""Exercise the strict-aligned Pillow runtime in memory and, optionally, ext4."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path

import PIL
from PIL import Image, features
from PIL import _imaging


EXPECTED_VERSION = "12.3.0"
WIDTH = 17
HEIGHT = 13
PIXEL = (23, 101, 211)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def assert_runtime_origin() -> None:
    root_text = os.environ.get("CPYTHON_ROOT", "")
    paths = [Path(PIL.__file__).resolve(), Path(_imaging.__file__).resolve()]
    for path in paths:
        path_text = str(path)
        assert "/tools/" not in path_text, path_text
        assert "/persist/python/user/" not in path_text, path_text
    if root_text:
        root = Path(root_text).resolve()
        for path in paths:
            assert path.is_relative_to(root), (path, root)


def encode_and_decode() -> tuple[bytes, bytes]:
    source = Image.new("RGB", (WIDTH, HEIGHT), PIXEL)

    png_buffer = io.BytesIO()
    source.save(png_buffer, format="PNG")
    png = png_buffer.getvalue()
    with Image.open(io.BytesIO(png)) as decoded:
        decoded.load()
        assert decoded.mode == "RGB", decoded.mode
        assert decoded.size == (WIDTH, HEIGHT), decoded.size
        assert decoded.getpixel((8, 6)) == PIXEL

    jpeg_buffer = io.BytesIO()
    source.save(jpeg_buffer, format="JPEG", quality=91)
    jpeg = jpeg_buffer.getvalue()
    with Image.open(io.BytesIO(jpeg)) as decoded:
        decoded.load()
        assert decoded.mode == "RGB", decoded.mode
        assert decoded.size == (WIDTH, HEIGHT), decoded.size

    return png, jpeg


def write_and_reopen(output_dir: Path, png: bytes, jpeg: bytes) -> dict[str, str]:
    if not output_dir.is_absolute():
        raise SystemExit("--output-dir must be an absolute path")
    output_text = str(output_dir)
    if output_text == "/tools" or output_text.startswith("/tools/"):
        raise SystemExit("refusing to use the P3 /tools backup tree")
    if output_text == "/persist/python/user" or output_text.startswith(
        "/persist/python/user/"
    ):
        raise SystemExit("native-runtime smoke data must not enter the Python user site")

    output_dir.mkdir(parents=True, exist_ok=True)
    results: dict[str, str] = {}
    for suffix, payload, expected_format in (
        ("png", png, "PNG"),
        ("jpg", jpeg, "JPEG"),
    ):
        path = output_dir / f"aligned-pillow-smoke.{suffix}"
        with path.open("wb") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        with Image.open(path) as decoded:
            decoded.load()
            assert decoded.format == expected_format, decoded.format
            assert decoded.size == (WIDTH, HEIGHT), decoded.size
        results[suffix] = str(path)
    return results


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--output-dir",
        type=Path,
        help="also persist and reopen PNG/JPEG files (use P4 /persist on board)",
    )
    args = parser.parse_args()

    assert PIL.__version__ == EXPECTED_VERSION, PIL.__version__
    assert features.check("zlib") is True
    assert features.check("jpg") is True
    assert_runtime_origin()
    png, jpeg = encode_and_decode()

    result: dict[str, object] = {
        "pillow": PIL.__version__,
        "pillow_module": str(Path(PIL.__file__).resolve()),
        "imaging_module": str(Path(_imaging.__file__).resolve()),
        "features": {"jpeg": True, "zlib": True},
        "png_bytes": len(png),
        "png_sha256": sha256(png),
        "jpeg_bytes": len(jpeg),
        "jpeg_sha256": sha256(jpeg),
    }
    if args.output_dir is not None:
        result["files"] = write_and_reopen(args.output_dir, png, jpeg)
    print("aligned-pillow-smoke-ok " + json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
