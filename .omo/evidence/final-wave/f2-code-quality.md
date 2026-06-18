rv64 Build PASS | la64 Build PASS | Files 5 clean/5 issues | VERDICT: REVIEW

Notes:
- rv64/la64 builds both passed serially.
- `os/src/fs/mod.rs` has one LSP warning: unnecessary braces in a `pub use`.
- No `lang_items.rs` edits detected in working tree.
- Main review issues:
  - `os/src/fs/ext4/ext4fs.rs`: `write_at()` ignores `invalidate_range()` failure.
  - `os/src/fs/ext4/layout.rs`: `Drop::drop()` holds `new_page_cache` lock during `writeback_all()`.
  - `os/src/syscall/fs.rs`: `sys_fsync()` is stubbed; `sys_umount2()` is fake.
  - `os/src/fs/page_cache_test.rs`: stale `DirtyBlockDevice` wording in comment.
