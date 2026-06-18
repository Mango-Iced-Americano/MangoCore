## 2026-05-17

- PageCache 全局 flush 使用 `Weak<PageCache>` 注册表，避免全局注册表延长缓存生命周期。
- `sys_sync()` 暂按需求保留成功 stub；真正的全局写回先由 ext4 `on_umount()` 触发，`syncfs()` 返回 `ENOSYS`。
- ext4 `sync()` 同时写回脏页和 inode 元数据；`datasync()` 只写回脏页。
