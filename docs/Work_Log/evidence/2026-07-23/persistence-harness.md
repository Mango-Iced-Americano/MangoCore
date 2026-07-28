# rv64 virt image lifecycle and fsck harness

## Active image

- Path in the development container: `/app/sdcard-rv.img`
- Observed size: `4294967296` bytes (4 GiB)
- The `fs-img-dir` directory stores the compressed seed (`sdcard-rv.img.xz`); it does **not** contain an active `sdcard-rv.img` in this environment.
- Initialize/reset the active image from the seed when needed:

```sh
xz -dkc fs-img-dir/sdcard-rv.img.xz > sdcard-rv.img
```

## Config injection lifecycle

For rv64 virt, `make conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt ...` expands to:

```sh
ARCH=rv64 BLK_MODE=virt CONF_FILE=../os_test_iozone.conf \
  IMAGE_PATH= AUTO_REBUILD_MEM=1 MODE=release LOG= \
  ./inject_os_test_conf.sh
```

`os/inject_os_test_conf.sh` resolves that invocation to the persistent image
`/app/sdcard-rv.img`, then runs exactly:

```sh
e2fsck -fy "${IMAGE_PATH_ABS}" 2>&1 || true
```

before using `debugfs -w` to remove `/os_test.conf`, write the selected config,
and stat it. The fsck step is intentional: lwext4 may leave stale metadata
checksums on bitmap writes, which otherwise makes `debugfs` reject the image.

QEMU mutates this same image. Before every follow-up run, use `conf-inject` so
that fsck performs journal recovery/repair and the intended `/os_test.conf` is
re-injected. Preserve the compressed seed separately; do not mistake it for the
live image.
