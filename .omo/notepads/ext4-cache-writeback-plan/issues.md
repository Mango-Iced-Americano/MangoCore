## 2026-05-17

- `flush_all_page_caches()` 是 best-effort：单个 PageCache 写回错误被忽略，适合 umount 兜底但不能向 syscall 返回精确错误。
