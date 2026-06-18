# F4 Scope Fidelity Check — ext4-cache-writeback-plan

Date: 2026-05-17
Branch: `refactor/fs`

## Verdict summary

`Tasks [8/21 compliant] | Creep [3 issues] | Missing [13 items] | VERDICT: REJECT`

## Inputs checked

- Plan: `.sisyphus/plans/ext4-cache-writeback-plan.md` T1-T21.
- Committed branch diff: `GIT_MASTER=1 git diff main..HEAD --stat` / `--name-status`.
- Working tree diff: `GIT_MASTER=1 git status --short`, `GIT_MASTER=1 git diff --stat`, `GIT_MASTER=1 git diff --name-status`.
- Source searches:
  - `DirtyBlockDevice|dirty_block_device|flush_dirty_blocks|if let Some([^\n]*ext4` under `os/src/**/*.rs`.
  - `journal|Journal|JBD|jbd|transaction|commit_block|revoke` under `os/src/fs/ext4/**/*.rs`.
  - `sys_fsync|sys_fdatasync|sys_sync|sys_umount2|SYSCALL_*SYNC|SYSCALL_UMOUNT2` under `os/src/**/*.rs`.

## Git state evidence

### Working tree (`git status --short`)

Uncommitted source changes are present and were included in this audit:

- Modified: `os/src/drivers/block/virtio_blk.rs`, `os/src/drivers/block/virtio_blk_pci.rs`, multiple `os/src/fs/ext4/*`, `os/src/fs/fat32/*`, `os/src/fs/mod.rs`, `os/src/fs/page_cache.rs`, `os/src/fs/vfs/file.rs`, `os/src/fs/vfs/mount.rs`, `os/src/net/socket/mod.rs`, `os/src/syscall/process.rs`, `user/src/bin/initproc.rs`.
- Deleted: `os/src/fs/cache.rs`, `os/src/fs/ext4/dirty_block_device.rs`.
- Modified despite project guardrail: `os/src/lang_items.rs`, `user/src/lang_items.rs`.
- Untracked: `os/src/fs/page_cache_test.rs`.

Working tree diff stat: 24 files, 354 insertions, 986 deletions.

### `main..HEAD` committed diff

Committed branch diff is very broad: 115 files changed, 14,875 insertions, 8,566 deletions. It includes VFS migration, ext4, fat32, devfs/ramfs, syscall, networking, memory-management, task/signal/thread, user tests, scripts and docs.

Notable committed entries relevant to this F4 check:

- `A os/src/fs/ext4/dirty_block_device.rs` in `main..HEAD` (later deleted only in working tree).
- `A os/src/fs/page_cache.rs`, `A os/src/fs/vfs/*`, large `M os/src/syscall/fs.rs`.
- Broad non-plan areas: `os/src/mm/*`, `os/src/net/*`, `os/src/task/*`, `os/src/hal/*`, `user/src/bin/fs_test.rs`, `user/src/syscall.rs`.

## Guardrail checks

### DirtyBlockDevice final architecture

Current working tree search result:

- Only normal-source match is a comment in `os/src/fs/page_cache_test.rs:4` saying the tests do not depend on DirtyBlockDevice.
- `os/src/fs/ext4/dirty_block_device.rs` is deleted in the working tree.
- `os/src/fs/ext4/ext4fs.rs` no longer has a `dirty_bd` field/import in the working tree.

Assessment: current working tree does **not** keep DirtyBlockDevice on the normal path. However, `main..HEAD` still contains the earlier added DirtyBlockDevice file, so the branch must commit the deletion before final review.

### Syscall ext4 special-casing

Search result for `if let Some(...ext4`, `flush_dirty_blocks`, `dirty_block_device` under current `os/src/**/*.rs` found no syscall-layer ext4 downcast/special-case. This guardrail is clean.

### Full journal scope creep

Search result under `os/src/fs/ext4` found only pre-existing superblock fields:

- `journal_uuid`, `journal_inode_number`, `journal_dev`, `journal_backup_type`, `journal_blocks` in `superblock.rs`.

No JBD/full journal machinery (`transaction`, `commit_block`, `revoke`, etc.) was found. This guardrail is clean.

## Task compliance matrix

| Task | Scope check | Result |
|---|---|---|
| T1 baseline evidence/harness | Evidence files exist (`task-1-*`) and describe baseline gaps. | COMPLIANT |
| T2 VFS sync trait + File bridge | `IndexNode::sync/datasync` defaults exist, but current `vfs::File` has no sync/datasync bridge and syscall cannot call it. | MISSING |
| T3 PageCache registry/writeback API | Per-cache `writeback_all/range/page` exists, but no global registry was found. | MISSING |
| T4 BlockCache metadata flush contract | Old `fs/cache.rs` is deleted and no clear replacement metadata flush contract is wired to FS sync. | MISSING |
| T5 ext4 ownership/DirtyBlockDevice boundary map | Evidence exists (`task-5-*`). | COMPLIANT |
| T6 syscall surface audit | Evidence exists (`task-6-*`), and audit findings are visible. | COMPLIANT |
| T7 generic fsync/fdatasync/sync bridge | Current `sys_fsync` only validates fd and returns success; no `sys_fdatasync`/`sys_sync` wiring found. | MISSING |
| T8 real umount2 -> MountFS::umount | Current `sys_umount2` logs `fake implementation!` and returns success. | MISSING |
| T9 ext4 inode sync/datasync/close metadata semantics | No ext4 override for `sync`/`datasync` found; Drop only attempts per-inode page writeback. | MISSING |
| T10 ext4 data writes through PageCache | Current `Ext4OSInode::write_at` calls `self.ext4fs.write_at(...)` direct I/O then invalidates PageCache. | MISSING |
| T11 two-phase cached write | Since T10 still direct-writes, the planned two-phase cached write protocol is not present. | MISSING |
| T12 Ext4PageCacheBackend avoids DirtyBlockDevice double-deferral | Current backend writes directly via `fs.block_device.write_block`; no DirtyBlockDevice path found. | COMPLIANT |
| T13 metadata flush in FileSystem::sync_fs | `Ext4FileSystem` does not override `sync_fs`/`on_umount`; default no-op remains. | MISSING |
| T14 remove DirtyBlockDevice normal path | Working tree deletes DirtyBlockDevice and ext4 references. | COMPLIANT |
| T15 global PageCache writeback/reclaim | No global registry/writeback/reclaim trigger found. | MISSING |
| T16 persistence scenarios | Evidence exists, but `task-16-binbash-persistence.txt` states writes still go through DirtyBlockDevice and only records `/bin/bash` rv64-style evidence, not full planned scenarios. | MISSING |
| T17 busybox/preload regression guard | Evidence exists (`task-17-busybox-install-persistence.txt`). | COMPLIANT |
| T18 rv64 integration/persistence audit | No `task-18-*` evidence found; only `final-qa/rv64-run.log` exists. | MISSING |
| T19 la64 integration/persistence audit | No `task-19-*` evidence and no la64 final QA evidence found. | MISSING |
| T20 docs | Evidence exists (`task-20-doc-architecture.txt`) and docs changed in `main..HEAD`. | COMPLIANT |
| T21 stale cleanup | DirtyBlockDevice file/export removed in working tree; remaining reference is a test comment. | COMPLIANT |

Compliant tasks counted: T1, T5, T6, T12, T14, T17, T20, T21 = 8/21.

## Scope creep / pollution issues

1. `main..HEAD` includes broad non-plan changes in memory management, networking, task/signal/thread, HAL, and user syscall/test areas. Some may be pre-existing branch work, but relative to `main..HEAD` they cannot all be mapped to T1-T21 or evidence/docs for this ext4 writeback plan.
2. Working tree modifies `os/src/lang_items.rs` and `user/src/lang_items.rs`, conflicting with the project guardrail to edit `.rv`/`.la` variants instead of generated `lang_items.rs` files.
3. Committed branch history still adds `os/src/fs/ext4/dirty_block_device.rs`; although the working tree deletes it, final architecture is not representable by committed `HEAD` until the deletion is committed.

## Missing items requiring rejection

- Generic fsync/fdatasync/sync bridge is absent; `sys_fsync` is still effectively no-op.
- `sys_umount2` is still fake.
- ext4 `sync`/`datasync` and filesystem `sync_fs`/`on_umount` are not wired.
- ext4 writes still direct-write and invalidate PageCache instead of dirtying PageCache.
- No global PageCache registry/writeback/reclaim path found.
- Persistence evidence is incomplete and partially documents DirtyBlockDevice usage.
- rv64/la64 final integration evidence is incomplete, with la64 missing.

## Final verdict

`Tasks [8/21 compliant] | Creep [3 issues] | Missing [13 items] | VERDICT: REJECT`
