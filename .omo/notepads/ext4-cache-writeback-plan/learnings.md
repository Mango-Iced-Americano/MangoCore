## 2026-05-17

- ext4 VFS `write_at` 需要把 PageCache 视为数据源：先更新 inode size/timestamp，再 `pc.write()`，不要 direct write 后 invalidate。
- `fsync` 是否真实生效取决于 `MountFSInode` 是否转发 `sync()`；只在底层 inode 实现 `sync()` 不够。
- 当前 `MountFS::add_mount()` 只记录 inode_id，没有设置子 MountFS 的 self_mountpoint；路径解析穿越挂载点时补充记录，才能让 `umount()` 从父挂载表移除自己。
