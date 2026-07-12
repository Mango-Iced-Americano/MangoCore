---
title: "FAT32 文件系统"
module: fs/fat32
category: fs
status: draft
owner: MangoCore Team
last_updated: "2026-07-12"
code_paths:
  - "os/src/fs/fat32/"
entry_points:
  - "EasyFileSystem"
  - "FatInode"
  - "DiskInodeType"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "open*"
    - "read*"
    - "write*"
    - "stat*"
    - "mkdir*"
    - "rmdir*"
    - "rename*"
    - "getdents*"
  oscomp:
    - "basic"
    - "busybox"
    - "lua"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/03_fs/page-cache.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/ltp/ltp_fs_plan.md"
---

## 概述

FAT32 文件系统是 MangoCore 对 FAT32 标准的实现，主要负责引导分区和 EFI 分区的读写访问。它通过块设备接口与底层存储交互，支撑内核在 QEMU 环境下读取启动配置、加载引导文件，以及挂载测试镜像中的共享数据分区。

实现定位为轻量级 FAT32 读写驱动，不对标生产环境的高性能需求，但必须保证数据一致性。核心设计围绕三个要点：**块设备抽象上的 BPB 解析**、**FAT 表驱动的簇链遍历**、**长/短文件名目录项的统一管理**。

## 模块结构

| 子模块 | 文件 | 职责 |
|--------|------|------|
| efs | `fat32/efs.rs` | `EasyFileSystem`：文件系统对象，BPB 加载、块设备封装、FileSystem trait 实现 |
| fat_inode | `fat32/fat_inode.rs` | `FatInode`：inode 实现，包含文件内容管理、目录操作、读写和大小调整 |
| layout | `fat32/layout.rs` | 磁盘数据结构：BPB、FATShortDirEnt、FATLongDirEnt、FATDirEnt 联合体 |
| dir_iter | `fat32/dir_iter.rs` | 目录迭代器 `DirIter` 和 `DirWalker`，支持 5 种遍历模式和两个方向 |
| bitmap | `fat32/bitmap.rs` | `Fat`：FAT 表内存表示，簇分配/释放/遍历 |
| mod | `fat32/mod.rs` | 模块入口，重导出关键类型 |

## 关键数据结构

### EasyFileSystem

FAT32 文件系统的核心结构体。它在 `open()` 时直接读取块设备的 0 号扇区，解析 BPB（BIOS Parameter Block）以获取文件系统的布局参数。所有数据簇的访问都基于 `data_area_start_block` 偏移量和 `sec_per_clus` 每簇扇区数计算得出。

```rust
pub struct EasyFileSystem {
    pub block_device: Arc<dyn BlockDevice>,
    pub fat: Fat,
    pub data_area_start_block: u32,
    pub root_clus: u32,
    pub sec_per_clus: u8,
    pub byts_per_sec: u16,
    __self_ref: spin::Mutex<Option<Weak<EasyFileSystem>>>,
}
```

- `block_device`：底层块设备，通过 `read_block`/`write_block` 访问扇区。
- `fat`：FAT 表的内存抽象，负责簇链管理和空闲簇分配。
- `root_clus`：根目录的起始簇号，通常为 2。
- `__self_ref`：弱引用自指针，用于 `FileSystem::root_inode()` 的回调。

### FatInode

FatInode 对应 FAT32 中的一个文件或目录。它不直接维护磁盘块号列表，而是记录该文件占用的 `clus_list`（簇号序列），通过簇链计算出对应的磁盘扇区位置。

```rust
pub struct FatInode {
    inode_lock: RwLock<InodeLock>,
    file_content: RwLock<FileContent>,
    new_page_cache: Mutex<Option<Arc<NewPageCache>>>,
    self_weak: Mutex<Option<Weak<FatInode>>>,
    file_type: Mutex<DiskInodeType>,
    parent_dir: Mutex<Option<(Arc<Self>, u32)>>,
    fs: Arc<EasyFileSystem>,
    time: Mutex<InodeTime>,
    deleted: Mutex<bool>,
}
```

- `file_content` 包含 `size`（文件大小）和 `clus_list`（簇号数组）。
- `new_page_cache` 是懒初始化的 PageCache 实例，用于缓存文件数据和加速读写。
- `parent_dir` 记录父目录 inode 和目录项偏移，用于修改操作同步短目录项及目录页。

### Fat / FAT 表

FAT 表是 FAT32 文件系统的核心索引结构。`Fat` 结构体维护表头的起始块位置和总计条目数，通过块设备直接读写 FAT 扇区：

- `get_next_clus_num(current)`：读取当前簇号对应的 FAT 表项，返回下一个簇号。
- `get_all_clus_num(start)`：从起始簇号开始沿链遍历，收集全部簇号。
- `alloc(n, last)`：分配 n 个空闲簇，更新前驱簇的链指针。
- `free(list, last)`：释放簇链，标记 FAT 表项为空。

空闲簇管理使用 `vacant_clus` 缓存队列（最大 64 个已释放簇号）加速分配，并维护 `hint` 指针避免每次都从头扫描 FAT 表。

## FAT 布局

FAT32 卷的磁盘布局如下：

```
+---------------------------+
| 保留区 (Boot Sector + BPB) |
|   BPB @ LBA 0            |
|   FSInfo @ LBA 1          |
|   备份引导 @ LBA 6         |
+---------------------------+
| FAT 表 1                  |
|   (每个簇项 4 字节)        |
+---------------------------+
| FAT 表 2 (镜像, 可选)      |
+---------------------------+
| 根目录数据区               |
|   (root_clus 指向的簇)     |
+---------------------------+
| 数据区                     |
|   簇 2 ... 簇 N           |
+---------------------------+
```

每个数据簇的大小由 `sec_per_clus * byts_per_sec` 决定，SD 卡场景下通常为 8 * 512 = 4096 字节。簇号从 2 开始，簇 0 和簇 1 为保留值。

## 目录项管理

### 短文件名 (8.3)

`FATShortDirEnt` 是对应 FAT 短目录项的磁盘结构，固定 32 字节。`name` 字段为 11 字节（8 字节基本名 + 3 字节扩展名），空间填充 ASCII 空格（0x20）。属性字节 `attr` 区分目录（`AttrDirectory`）和文件（`AttrArchive`）。首簇号由 `fst_clus_hi` 和 `fst_clus_lo` 拼接得到。

### 长文件名 (VFAT)

`FATLongDirEnt` 支持最长 255 字符的 Unicode 文件名，以 UTF-16LE 编码。每个长目录项携带 13 个字符，多个条目倒序排列在对应的短目录项之前。`ord` 字段标识序号，最高位 `0x40` 标记最后一个条目。实现中通过 `DirWalker` 将连续的长条目拼接为完整文件名。

### 目录遍历

`DirIter` 提供五种迭代模式：

| 模式 | 行为 |
|------|------|
| `Enum` | 遍历所有目录项（含已删除和末尾标记） |
| `Used` | 仅遍历有效条目（跳过已删除） |
| `Unused` | 仅遍历空闲条目 |
| `Long` | 仅遍历长文件名条目 |
| `Short` | 仅遍历短文件名条目 |

`DirWalker` 构建在 `DirIter` 之上，将长/短条目组合为一个 `(String, FATShortDirEnt)` 元组供上层使用。

`hint` 机制记录目录文件中第一个空闲字节的偏移量，新建文件时直接从 `hint` 处开始分配目录项，避免从头扫描。

## 文件操作

### 读操作

`read_at_block_cache_wlock(offset, buf)` 读取文件指定偏移处的数据。流程如下：

1. 从 `file_content` 获取文件大小，裁剪读范围。
2. 通过 `get_new_page_cache()` 获取或初始化 PageCache。
3. 委托 PageCache 读取数据，PageCache 调用 `FatPageCacheBackend` 将文件偏移转换为簇号和扇区号。
4. PageCache 回源时通过 `block_id_for_offset` 计算物理块位置：`簇索引 = page_index * blocks_per_page / sec_per_clus`。

### 写操作

`write_at_block_cache_lock(offset, buf)` 支持按偏移写入。当写入超出当前文件大小时自动触发 `modify_size_lock` 扩展文件：

1. 计算新旧大小差 `diff_len`。
2. 若需扩容，调用 `modify_size_lock` 分配新簇并追加到 `clus_list`。
3. 写入数据到 PageCache，标记脏页。

文件首次分配簇或大小变化后，write/resize 路径必须立即更新父目录中的短目录项；`sync()` 还要依次写回文件数据页、短目录项和父目录页。不能把这一步延迟到 Rust `Drop`，因为对象生命周期不是文件系统提交协议。

### 大小调整

`modify_size_lock(diff, clear)` 处理文件伸缩：

- 扩容时通过 `alloc_clus` 从 FAT 表分配新簇，追加到 `clus_list`。
- 缩容时通过 `dealloc_clus` 释放尾部簇，修改 FAT 表项。
- 目录文件的大小以簇为单位对齐（至少保留一个簇）。

### 删除与回收

`unlink_lock(delete)` 从父目录中删除目录项并可选释放数据簇。删除操作将目录项首字节标记为 `0xE5`，同时更新父目录的 `hint` 指针。若 `delete=true`，`Drop` 时实际调用 `dealloc_clus` 回收到 FAT 表。

### 重命名

FAT 不支持硬链接，因此不能使用 VFS 默认的 `link + unlink` rename。当前同目录 rename 读取源短目录项，保留首簇、文件大小、属性和时间字段，只替换短名并生成新的 VFAT 长名项；新目录项创建成功后删除旧项并显式写回父目录，删除失败则回滚新项。该路径不复制数据，也不分配或释放源文件的数据簇。

### 目录事务与 inode 别名

当前 `EasyFileSystem::root_inode()` 和 `FatInode::find()` 可能为同一个磁盘对象构造独立 inode/PageCache。目录创建若只把父目录项留在某个 PageCache 中，后续通过另一份根 inode 执行 `rmdir` 会直接从磁盘读到 `ENOENT`。因此 `fat_do_create()` 在返回前显式写回父目录；创建目录时还要先写回 `.`、`..` 和结束标记。unlink/rmdir 同样在成功返回前提交所属目录页。

stale inode 的 `Drop` 只允许写回自身脏数据和回收已删除 inode 的簇，不再更新父目录元数据。否则旧对象可能覆盖复用后的目录项，甚至复活已删除文件。长期方案仍是为 FAT inode/page cache 建立规范化缓存；在此之前，所有元数据修改必须以显式提交点为边界。

## PageCache 集成

FAT32 通过 `FatPageCacheBackend` 接入通用 PageCache 层。该后端将文件偏移映射到物理扇区的逻辑为：先通过 `inode.file_content.clus_list` 获取簇号数组，再计算簇内偏移，最终得到块设备扇区号。读时若缓存缺失则回源读取整页（4096 字节），写操作通过 PageCache 的脏页回写机制持久化。

## 限制与差异

- **大小写不敏感**：`find_local_lock` 使用 `eq_ignore_ascii_case` 匹配，文件名大小写等价。
- **不支持符号链接**：FAT32 标准无 symlink 概念，`symlink()` 返回错误。
- **不支持硬链接**：FAT32 目录项不提供多链接计数。
- **无权限管理**：文件属性仅保留 FAT 标准的只读/隐藏/系统/归档标记，不支持 Unix rwx 权限；`chmod` 和 `chown` 返回空成功。
- **时间戳精度**：FAT 时间戳精度为 2 秒，目录项 Drop 时自动写回修改时间。
- **最大文件大小**：受 FAT32 4 GiB 单文件上限约束（`file_size` 字段为 u32）。
- **rename 范围**：当前仅支持同目录且目标不存在的文件/目录 rename；跨目录移动和覆盖已有目标返回显式错误，尚未实现双目录与目标回滚事务。

## 测试映射

| 特性 | 覆盖方式 | 测试组 | 状态 |
|------|----------|--------|------|
| 文件创建/删除 | basic/busybox touch & rm | OSComp basic | pass |
| 目录遍历 | ls / getdents64 syscall | OSComp basic, busybox | pass |
| 文件读写 | dd / cat / echo | OSComp basic, busybox | pass |
| 长文件名创建 | lua 文件 I/O 测试 | OSComp lua | pass |
| 同目录文件/目录重命名 | mv / rename syscall | OSComp busybox | pass（实板 FAT32 scratch） |
| FAT 表空间耗尽 | 大文件写入达容量上限 | 手动测试 | pass |
| 目录树层级 | mkdir -p 嵌套路径 | OSComp busybox | pass |

## 已知问题

1. **大小写处理不完整**
   - 现象：`find_local_lock` 使用 `eq_ignore_ascii_case` 匹配，但短文件名内部始终大写存储。若文件系统上同时存在 "File" 和 "file"（FAT32 规范禁止但偶有工具生成），行为未定义。
   - 根因：FAT32 规范要求文件名大小写不敏感，但实现未做冲突检测。
   - 影响：边缘场景下可能匹配到预期之外的文件。
   - 建议：在 `create` 路径增加重名检测，优先匹配精确大小写。

2. **短文件名冲突时回退策略简单**
   - 现象：生成 8.3 短名时若基础名超过 6 个字符，截断至 6 字符加 `~N` 数字尾；若 1~9 全冲突则退化为伪随机 16 进制尾缀。
   - 根因：`gen_short_name_numtail` 中的伪随机算法使用固定种子 `jiffies = 19382022`，不具备跨实例唯一性。
   - 影响：高并发创建同名文件场景下短名可能重复。
   - 建议：引入随机数种子或递增计数器。

3. **FAT 表的线程安全**
   - 现象：`Fat` 的 `vacant_clus` 和 `hint` 使用 `Mutex` 保护，但 `get_next_clus_num` 和 `write_fat_entry` 直接操作块设备，没有全局锁。
   - 根因：单核环境下竞争条件不会触发，但设计上未考虑多核扩展。
   - 影响：在多核环境中并发分配/释放簇可能导致 FAT 表损坏。
   - 建议：在 `EasyFileSystem` 层面引入 FAT 操作互斥锁。

4. **文件截断时空闲簇未立即回收**
   - 现象：`modify_size_lock` 缩小文件大小时仅缩短 `clus_list`，实际释放簇的操作延迟到文件关闭或 `unlink` 时通过 `dealloc_clus` 执行。
   - 根因：FAT 表更新聚焦于 `Drop` 时的批量处理，`ftruncate` 路径未主动回收。
   - 影响：截断后磁盘空间未即时释放，极端情况短时空间膨胀。
   - 建议：在 `resize` 路径增加即时回收逻辑。
