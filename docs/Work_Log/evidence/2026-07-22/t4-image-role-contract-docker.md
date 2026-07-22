# T4 image-role contract Docker evidence

- UTC date: 2026-07-22
- Host worktree: `/home/pxy/projects/MangoCore-cleanup`
- Repository revision during fixture: `a2a19e15-dirty`
- Container ID: `c238e449081e4c68d07b193d7e7e7c357406503eaab22305cc763a4d9c2e1161`
- Image: `zhouzhouyi/os-contest:20260510`
- Mount: `/home/pxy/projects/MangoCore-cleanup -> /app`

## Commands and status

```sh
docker compose exec -T os-dev sh -n os/inject_os_test_conf.sh
docker compose exec -T os-dev sh -n scripts/test-image-role-contract.sh
docker compose exec -T os-dev python3 -m py_compile scripts/image_roles.py
docker compose exec -T os-dev sh scripts/test-image-role-contract.sh
for fixture in remaining-consumer cross-arch-derived symlink-alias hardlink-alias basename-alias make-override mktemp-failure unmount-failure; do
    docker compose exec -T os-dev sh scripts/test-image-role-contract.sh --fixture "$fixture"
done
```

All commands exited `0`. The fixture output was:

```text
PASS: image role contract
PASS: fixture rejected: remaining-consumer
PASS: fixture rejected: cross-arch-derived
PASS: fixture rejected: symlink-alias
PASS: fixture rejected: hardlink-alias
PASS: fixture rejected: basename-alias
PASS: fixture rejected: make-override
PASS: fixture rejected: mktemp-failure
PASS: fixture rejected: unmount-failure
```

This is a static, no-QEMU contract run. It did not use an `os_test.conf` image
injection payload, so no QEMU serial log, config checksum, or QEMU head/tail
artifact exists. The alias and cross-architecture injection fixtures use empty
temporary files and command sentinels; they assert `cp`, `e2fsck`, and `debugfs`
were never invoked before rejection.
