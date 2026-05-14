# VFS 重构迁移计划

## 目标

用受 DragonOS 启发的分层 VFS 架构全面替换旧的 VFS 接口，最终删除所有旧代码。

## 新 VFS 架构（对标 DragonOS）

```
syscall 层
    ↓
File (fd 层：offset、flags、mode)
    ↓
IndexNode trait (inode 操作：read_at/write_at/find/create/...)
    ↓
MountFS/MountFSInode (挂载层：跨 FS 路径解析)
    ↓
FileSystem trait (具体 FS：root_inode/info/name)
    ↓
PageCache (页缓存)
```

## 当前进度

### ✅ 已完成

| 项目 | 状态 | 说明 |
|------|------|------|
| VFS 核心抽象 | ✅ | `IndexNode`, `FileSystem`, `File`, `FdTable`, `MountFS`, `MountFSInode` |
| 新 PageCache | ✅ | 状态机：Loading→UpToDate↔Dirty→Writeback→UpToDate |
| `vfs_lookup()` 路径解析 | ✅ | 含 symlink 跟随（修复 FAT32 symlink 读取用 `read_link()`） |
| `vfs_lookup_parent()` | ✅ | 返回 (父目录, 文件名)，用于创建/删除操作 |
| `parse_path()` | ✅ | 标准化路径，处理 `.` 和 `..` |
| `VFS_ROOT` (MountFS) | ✅ | **直接使用 ext4/FAT32 的 FileSystem trait，无需 PlaceholderFS** |
| 8 个 syscall 迁移 | ✅ | openat, mkdirat, unlinkat, symlinkat, chdir, fstatat, statx, readlinkat |
| ext4: `FileSystem` trait | ✅ | `Ext4FileSystem` 实现新 `FileSystem` |
| ext4: `IndexNode` trait | ✅ | `Ext4OSInode` 实现新 `IndexNode` (read_at/write_at/find/create/...) |
| FAT32: `FileSystem` trait | ✅ | `EasyFileSystem` 实现新 `FileSystem`（含 `Arc::new_cyclic` 初始化 `__self_ref`） |
| FAT32: `IndexNode` trait | ✅ | `FatInode` 实现新 `IndexNode` (read_at/write_at/find/create/unlink/...) |
| 删除 `vfs_old.rs` | ✅ | VFS trait 搬迁到 `directory_tree.rs` 作为过渡 |
| 删除 `PlaceholderFS` | ✅ | `VFS_ROOT` 直接 downcast `FILE_SYSTEM` 到具体 FS 类型，不使用适配器 |
| 设备文件 `IndexNode` | ✅ | Null, Zero, Urandom, Teletype (Tty), Pipe 实现 `IndexNode`；新增共享 `DevFS` |
| SocketFile `IndexNode` | ✅ | `SocketFile` 实现 `IndexNode`（委托 `try_recv`/`try_send`/`poll`）；新增 `SocketFS` |
| 代码审查修复 | ✅ | 修复 FAT32 `fat_do_create` 锁顺序反转、truncate 后缓存失效、Pipe wake 重复等 |
| 删除 `OldFileIndexNode` 适配器 | ✅ | `adapters.rs` (437行) 完全删除，`vfs_lookup` symlink 读取简化 |
| 删除 `FilePageCacheBackend` | ✅ | `page_cache.rs` 中移除旧 File trait 桥接后端 (~50行) |

### ❌ 待完成

## Phase 1 ✅ 已完成
- `VFS_ROOT` 直接使用具体 FS（ext4/FAT32），`placeholder.rs` 已删除

## Phase 2 ✅ 已完成
- `OldFileIndexNode` 适配器已删除（`adapters.rs` 437行）
- 所有活跃类型都原生实现 `IndexNode`：ext4、FAT32、Null、Zero、Urandom、Tty、Pipe、SocketFile
- `FilePageCacheBackend` 已删除

## Phase 3: FAT32 迁移到新 IndexNode/FileSystem trait ✅ 已完成

## Phase 4: 设备文件和 Socket 迁移 ✅ 已完成

**仍需完成：**
- `Hwclock` → `IndexNode`
- `Pipe` → `IndexNode`
- Socket 文件 (`SocketFile`) → `IndexNode`

**修改文件：**
- `os/src/fs/mod.rs` — `vfs_lookup` 中的 symlink 读取移除 `downcast_ref::<OldFileIndexNode>` 分支
- `os/src/syscall/fs.rs` — `resolve_dirfd()` 不再用 `OldFileIndexNode::new_standalone()`

## Phase 5: Task 结构迁移（下一阶段）

**目标：** `TaskControlBlock` 使用新 `vfs::File` 和 `vfs::FdTable`。

**前置条件已满足：**
- 所有活跃类型（ext4、FAT32、Null、Zero、Urandom、Tty、Pipe、SocketFile）都原生实现 `IndexNode`
- 旧适配器（`OldFileIndexNode`、`PlaceholderFS`、`FilePageCacheBackend`）已删除
- `vfs::File` 支持 `map_to_kernel_space()` 用于 ELF 加载，`FdTable` 有 `len()` 方法

**修改文件：**
- `os/src/task/task.rs` — `FsStatus.working_inode: Arc<vfs::File>`, `exe: Arc<Mutex<vfs::File>>`
- `os/src/task/mod.rs` — `INITPROC` 创建, `do_exit` fd 清理
- `os/src/task/elf.rs` — `load_elf_interp` 用新 VFS

**关键挑战：** 改 `task.files` 类型会触发连锁反应 — 所有 fd 操作 syscall、socket、poll 代码都需要同步修改。

## Phase 6: 所有 syscall 迁移到新 FdTable/File

**目标：** 所有 fd 操作 syscall 使用新 `vfs::FdTable` 和新 `vfs::File`。

**涉及 syscall (~20 个)：** read, write, close, dup, lseek, fstat, getdents64, ioctl, pipe2, ppoll, ...

## Phase 7: 最终清理

**目标：** 删除所有旧 VFS 文件。

| 文件 | 行数 | 状态 |
|------|------|------|
| `os/src/fs/vfs/placeholder.rs` | ~82 | ✅ 已删除 |
| `os/src/fs/vfs/adapters.rs` | ~437 | ✅ 已删除 |
| `os/src/fs/page_cache.rs` FilePageCacheBackend | ~50 | ✅ 已删除 |
| `os/src/fs/directory_tree.rs` | ~1130 | 待 Phase 5-6 |
| `os/src/fs/file_descriptor.rs` | ~502 | 待 Phase 6 |
| `os/src/fs/file_trait.rs` | ~77 | 待 Phase 6 |
| `os/src/fs/filesystem.rs` | ~64 | 待 Phase 6 |
| `os/src/fs/inode.rs` | ~186 | 待 Phase 6 |

**已删除：~570 行 | 待删除：~1960 行**

## 当前文件结构

```
os/src/fs/
├── mod.rs              ← VFS_ROOT, vfs_lookup, parse_path
├── directory_tree.rs   ← 旧 VFS trait (过渡), FILE_SYSTEM, ROOT, 设备初始化
├── file_trait.rs       ← 旧 File trait (FAT32/设备/socket 仍需要)
├── file_descriptor.rs  ← 旧 FileDescriptor + 旧 FdTable
├── filesystem.rs       ← 旧 FileSystem 结构体, FS_Type, pre_mount
├── inode.rs            ← 旧 InodeTrait, DiskInodeType (FAT32 专用)
├── vfs/                ← 新 VFS 模块
│   ├── mod.rs          ← FileType, InodeMode, Metadata 等核心类型
│   ├── index_node.rs   ← IndexNode trait
│   ├── file.rs         ← 新 File + 新 FdTable
│   ├── file_system.rs  ← FileSystem trait
│   ├── mount.rs        ← MountFS + MountFSInode
│   ├── adapters.rs     ← OldFileIndexNode (待删除)
│   └── placeholder.rs  ← PlaceholderFS (待删除)
├── page_cache.rs       ← 新 PageCache (含 FilePageCacheBackend 待删除)
├── cache.rs            ← 旧 PageCache/BlockCacheManager
├── ext4/               ← ext4 模块
│   ├── ext4fs.rs       ← Ext4FileSystem: FileSystem + VFS(过渡)
│   └── layout.rs       ← Ext4OSInode: File + IndexNode
├── fat32/              ← FAT32 模块 (待迁移)
└── dev/                ← 设备文件 (待迁移)
```

## 设计原则

1. **外部接口尽量不动** — syscall 签名、task 字段名保持不变，内部实现可切换
2. **如果外部接口阻碍文件系统重构，可以改动**
3. **参照 DragonOS 的设计模式** — API 签名、架构分层对齐 DragonOS
4. **编译通过为最低标准** — 每个 Phase 完成后必须编译通过
5. **阶段性验证** — 用 QEMU 运行 basic+busybox 测试组验证功能
