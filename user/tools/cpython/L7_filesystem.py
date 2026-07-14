import os
import sys
import stat
import shutil
import tempfile
from contextlib import contextmanager

fail = 0
PREFIX = "[CPYTHON L7]"


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


def make_base_dir():
    candidates = [
        os.environ.get("CPYTHON_TEST_TMPDIR"),
        os.getcwd(),
        "/tools/tests/cpython",
        "/tmp",
    ]
    last_err = None
    for parent in candidates:
        if not parent:
            continue
        try:
            os.makedirs(parent, exist_ok=True)
            base = os.path.join(parent, f"cpython_l7_fs_{os.getpid()}")
            if os.path.exists(base):
                shutil.rmtree(base)
            os.mkdir(base)
            return base
        except Exception as e:
            last_err = e
    raise RuntimeError(f"cannot create test directory: {last_err}")


old_cwd = os.getcwd()
base = None
posix_base = None

try:
    base = make_base_dir()
    print(f"{PREFIX} base: {base}", flush=True)
    os.chdir(base)

    # FAT32 is the real-board persistent scratch filesystem and intentionally
    # remains the target for ordinary file I/O below. It cannot encode a POSIX
    # symlink type, so symlink-specific ABI checks use the kernel's ramfs.
    posix_base = os.path.join("/tmp", f"cpython_l7_posix_{os.getpid()}")
    if os.path.exists(posix_base):
        shutil.rmtree(posix_base)
    os.mkdir(posix_base)
    print(f"{PREFIX} posix base: {posix_base} (ramfs)", flush=True)

    with check("open write read close"):
        with open("basic.txt", "w", encoding="utf-8") as f:
            f.write("hello\n")
        with open("basic.txt", "r", encoding="utf-8") as f:
            data = f.read()
        assert data == "hello\n", data

    with check("O_CREAT O_TRUNC O_APPEND semantics"):
        with open("append.txt", "w", encoding="utf-8") as f:
            f.write("old-data")
        with open("append.txt", "w", encoding="utf-8") as f:
            f.write("new")
        with open("append.txt", "a", encoding="utf-8") as f:
            f.seek(0)
            f.write("+tail")
        with open("append.txt", "r", encoding="utf-8") as f:
            data = f.read()
        assert data == "new+tail", data

    with check("relative path resolution"):
        os.mkdir("rel")
        with open("rel/file.txt", "w", encoding="utf-8") as f:
            f.write("relative-ok")
        with open("rel/../rel/./file.txt", "r", encoding="utf-8") as f:
            data = f.read()
        assert data == "relative-ok", data

    with check("mkdir file inside rmdir"):
        os.mkdir("dir1")
        with open("dir1/inside.txt", "w", encoding="utf-8") as f:
            f.write("inside")
        with open("dir1/inside.txt", "r", encoding="utf-8") as f:
            assert f.read() == "inside"
        os.unlink("dir1/inside.txt")
        os.rmdir("dir1")
        assert not os.path.exists("dir1")

    with check("rename over existing file"):
        open("rename_src.txt", "w", encoding="utf-8").close()
        open("rename_dst.txt", "w", encoding="utf-8").close()
        os.rename("rename_src.txt", "rename_dst.txt")
        assert not os.path.exists("rename_src.txt")
        assert os.stat("rename_dst.txt").st_size == 0
        os.unlink("rename_dst.txt")

        # Reuse both names and freshly allocated FAT clusters. A one-shot
        # check misses stale inode/PageCache aliases when the allocator
        # returns a cluster that still contains an older target payload.
        for i in range(20):
            expected = f"source-{i:02d}"
            old_target = f"target-{i:02d}"
            with open("rename_src.txt", "w", encoding="utf-8") as f:
                f.write(expected)
            with open("rename_dst.txt", "w", encoding="utf-8") as f:
                f.write(old_target)
            os.rename("rename_src.txt", "rename_dst.txt")
            assert not os.path.exists("rename_src.txt")
            with open("rename_dst.txt", "r", encoding="utf-8") as f:
                assert f.read() == expected
            os.unlink("rename_dst.txt")

    with check("unlink file"):
        with open("unlink_me.txt", "w", encoding="utf-8") as f:
            f.write("gone")
        os.unlink("unlink_me.txt")
        assert not os.path.exists("unlink_me.txt")

    with check("symlink readlink and read through symlink"):
        target_path = os.path.join(posix_base, "target.txt")
        link_path = os.path.join(posix_base, "link.txt")
        with open(target_path, "w", encoding="utf-8") as f:
            f.write("target-data")
        os.symlink("target.txt", link_path)
        target = os.readlink(link_path)
        assert target == "target.txt", target
        with open(link_path, "r", encoding="utf-8") as f:
            assert f.read() == "target-data"

    with check("stat and lstat"):
        st_file = os.stat(target_path)
        st_link = os.lstat(link_path)
        st_follow = os.stat(link_path)
        assert stat.S_ISREG(st_file.st_mode)
        assert stat.S_ISLNK(st_link.st_mode)
        assert stat.S_ISREG(st_follow.st_mode)
        assert st_follow.st_size == len("target-data")

    with check("directory iteration listdir scandir"):
        os.mkdir("iterdir")
        for name in ["a.txt", "b.txt", "c.txt"]:
            with open(os.path.join("iterdir", name), "w", encoding="utf-8") as f:
                f.write(name)
        names1 = sorted(os.listdir("iterdir"))
        names2 = sorted(entry.name for entry in os.scandir("iterdir"))
        assert names1 == ["a.txt", "b.txt", "c.txt"], names1
        assert names2 == names1, names2

    with check("ftruncate"):
        with open("truncate.txt", "w+", encoding="utf-8") as f:
            f.write("abcdef")
            f.flush()
            f.truncate(3)
            f.seek(0)
            data = f.read()
        assert data == "abc", data
        assert os.stat("truncate.txt").st_size == 3

    with check("flush and fsync"):
        with open("fsync.txt", "w", encoding="utf-8") as f:
            f.write("sync-data")
            f.flush()
            os.fsync(f.fileno())
        with open("fsync.txt", "r", encoding="utf-8") as f:
            assert f.read() == "sync-data"

    with check("tempfile creation and cleanup"):
        tmp = tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=".",
            prefix="tmp_",
            delete=False,
        )
        tmp_name = tmp.name
        try:
            tmp.write("temp-data")
            tmp.close()
            assert os.path.exists(tmp_name)
            with open(tmp_name, "r", encoding="utf-8") as f:
                assert f.read() == "temp-data"
        finally:
            if os.path.exists(tmp_name):
                os.unlink(tmp_name)
        assert not os.path.exists(tmp_name)

    with check("open file unlink remains readable"):
        with open("open_unlink.txt", "w+", encoding="utf-8") as f:
            f.write("still-readable")
            f.flush()
            os.fsync(f.fileno())
            f.seek(0)
            os.unlink("open_unlink.txt")
            assert not os.path.exists("open_unlink.txt")
            assert f.read() == "still-readable"

finally:
    os.chdir(old_cwd)
    if base is not None:
        shutil.rmtree(base, ignore_errors=True)
    if posix_base is not None:
        shutil.rmtree(posix_base, ignore_errors=True)

print(f"{PREFIX} RESULT {'PASS' if fail == 0 else 'FAIL'}", flush=True)
sys.exit(fail)
