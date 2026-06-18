# Task 5 — ext4 写路径职责地图与 DirtyBlockDevice 删除边界

## 总结

当前 ext4 的 `Ext4FileSystem::block_device` 在 `open_ext4rs()` 中被设置为 `DirtyBlockDevice`，所以 ext4 的文件数据、目录项、inode、bitmap、extent、superblock 等写入都会先进入内存 dirty map，而不是直接进入真实块设备。目标架构必须把职责拆开：文件数据归 PageCache，元数据归 ext4/BlockCache flush，最终 I/O 归真实 BlockDevice。

## DirtyBlockDevice 边界

| 位置 | 当前行为 | 分类 | 目标 |
|---|---|---|---|
| `os/src/fs/ext4/ext4fs.rs:46` | `DirtyBlockDevice::new(block_device.clone())` 包装真实设备 | remove | T14 移除正常 mount/open path 包装 |
| `os/src/fs/ext4/ext4fs.rs:54` | `block_device: dirty_bd.clone()` | remove | `block_device` 应指向真实 `Arc<dyn BlockDevice>` |
| `os/src/fs/ext4/ext4fs.rs:55` | 保存 `dirty_bd` 字段 | remove | 删除字段或隔离 legacy |
| `os/src/fs/ext4/ext4fs.rs:64-65` | `flush_dirty_blocks()` 委托 dirty map | remove | T13 改为 BlockCache/PageCache/FileSystem flush |
| `os/src/fs/ext4/ext4fs.rs:755` | `sync_fs()` 调 `flush_dirty_blocks()` | temporary-dependency | T13 替换为 metadata flush + PageCache flush |
| `os/src/fs/ext4/dirty_block_device.rs` | block-device-wide dirty map | quarantine/remove | T14 后不在正常路径编译引用 |

## 文件数据写路径

| 位置 | 当前路径 | 问题 | 目标 owner |
|---|---|---|---|
| `os/src/fs/ext4/ext4fs.rs:411-434` `Ext4OSInode::write_at` | 调 `self.ext4fs.write_at()`，随后 invalidate PageCache | 写绕过 PageCache；读缓存只是被动失效 | T10：改为 PageCache dirty write |
| `os/src/fs/ext4/file.rs:484-617` `Ext4FileSystem::write_at` | 分配/查找 pblock，`Block::load_offset` 后 `sync_blk_to_disk(self.block_device.clone())` | `self.block_device` 现在是 DirtyBlockDevice；数据写入匿名 dirty map | T10/T11：PageCache 写；T12：writeback 写真实设备 |
| `os/src/fs/page_cache.rs:712-791` `Ext4PageCacheBackend` | `write_page()` 经 `fs.block_device.write_block()` | PageCache writeback 仍可能二次进入 DirtyBlockDevice | T12：backend 最终写真实 BlockDevice |

## inode / 文件大小 / metadata 写路径

| 位置 | 当前路径 | 写入内容 | 目标 owner |
|---|---|---|---|
| `os/src/fs/ext4/ext4_inode.rs:520-555` `sync_inode_to_disk` | read inode table block，改 inode 区间，`block_device.write_block()` | inode table block | T9/T13：ext4 metadata flush through BlockCache |
| `os/src/fs/ext4/ext4_inode.rs:642-671` `write_back_inode*` | 调 `sync_inode_to_disk(self.block_device.clone())` | inode metadata | T9/T13 |
| `os/src/fs/ext4/file.rs:608-613` | 扩展文件后 `set_size()` + `write_back_inode()` | i_size / inode metadata | T11 two-phase：预分配 extent → 提交 i_size → PageCache write |
| `os/src/fs/ext4/direntry.rs:897` | parent inode after directory update | parent directory metadata | T13 |

## 目录项写路径

| 位置 | 当前路径 | 写入内容 | 目标 owner |
|---|---|---|---|
| `os/src/fs/ext4/direntry.rs:511-591` `dir_add_entry` | 修改目录 block 后 `sync_blk_to_disk(self.block_device.clone())` | directory data block，属于 metadata | T13 BlockCache/ext4 metadata flush |
| `os/src/fs/ext4/direntry.rs:603-676` `try_insert_to_existing_block` | split/insert direntry 后 `block.sync_blk_to_disk()` | directory entry block | T13 |
| `os/src/fs/ext4/direntry.rs:744` 等 remove 路径 | 修改 dir block 后写回 | directory metadata | T13 |
| `os/src/fs/ext4/ext4fs.rs:568-611` `rename` | `dir_add_entry` + `dir_remove_entry` + parent link count write_back_inode | directory metadata + inode metadata | T13/T9 |

## block/inode bitmap、group descriptor、superblock、extent 写路径

| 位置 | 当前路径 | 写入内容 | 目标 owner |
|---|---|---|---|
| `os/src/fs/ext4/ext4fs.rs:212` | `self.block_device.write_block(bmp_blk, &data)` | bitmap block | T13 metadata flush / real BlockDevice after T14 |
| `os/src/fs/ext4/block_group.rs:268-275` | read-modify-write block group related block | group descriptor / metadata block | T13 |
| `os/src/fs/ext4/superblock.rs:233-263` | read/update/write superblock | superblock counters/flags | T13 `sync_fs` metadata phase |
| `os/src/fs/ext4/extent.rs:801,862,932` | extent block `sync_blk_to_disk()` | extent tree metadata | T11 allocation + T13 metadata flush |
| `os/src/fs/ext4/extent.rs:824,933,1406` | `write_back_inode()` | inode extent header / metadata | T11/T13 |
| `os/src/fs/ext4/balloc.rs:331,404` | `write_back_inode()` after allocation/free changes | inode metadata after block allocation | T11/T13 |

## 最终设备 I/O owner

目标架构中只有以下层可以触达真实 `BlockDevice::write_block()`：

1. PageCache backend writeback：写普通文件数据页。
2. BlockCache/ext4 metadata flush：写 inode table、bitmap、directory block、extent block、superblock/group descriptor。
3. 特殊初始化/直接设备路径：必须有明确注释，不允许通过 DirtyBlockDevice 隐式缓存。

## 后续任务依赖

- T10 依赖本表的“文件数据写路径”分类。
- T11 依赖本表的 i_size/extent 顺序分类。
- T12 依赖本表的 PageCache backend 边界。
- T14 依赖 DirtyBlockDevice 使用点分类，执行移除/隔离。
