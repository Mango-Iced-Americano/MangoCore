# procfs Implementation Plan

## Goal

为 oskernel2026-mango 实现 procfs（伪文件系统），仿照 DragonOS 设计，适配现有 VFS 架构。

## Architecture Overview

### 设计策略

鉴于 oskernel2026-mango 的 VFS 比 DragonOS 更简洁（无 `inherit_methods` 宏、无 `RwSem`、无 `SpecialNodeData`），采用**简化版 template 模式**：

```
ProcFS (FileSystem trait)
  └─ LockedProcInode (IndexNode trait)
       ├─ children: BTreeMap<String, Arc<dyn IndexNode>>
       ├─ content_fn: Option<fn(offset, len, buf) -> Result<usize, SyscallErr>>
       └─ metadata, parent, self_ref, fs
```

**与 DragonOS 的关键差异**：
- 不使用 `FileOps` trait → 使用函数指针 `content_fn`（更轻量）
- 不使用 `DirOps` trait → 使用静态条目表 + 直接 `BTreeMap` 操作
- 不使用 `inherit_methods` → 直接在 `IndexNode` impl 中手工实现
- 不使用 `SymOps` → symlink 用常规 inode + content_fn 存储目标路径

### 目录结构

```
os/src/fs/procfs/
├── mod.rs           # ProcFS + LockedProcInode + ProcInodeData + 初始化
├── files/
│   ├── mod.rs       # 重新导出所有 proc 文件构造函数
│   ├── version.rs   # /proc/version
│   ├── uptime.rs    # /proc/uptime
│   ├── meminfo.rs   # /proc/meminfo
│   ├── cpuinfo.rs   # /proc/cpuinfo
│   ├── mounts.rs    # /proc/mounts
│   ├── stat.rs      # /proc/stat
│   ├── loadavg.rs   # /proc/loadavg
│   └── self_.rs     # /proc/self (symlink → /proc/<pid>)
├── pid/
│   ├── mod.rs       # /proc/<pid>/ 目录实现
│   ├── status.rs    # /proc/<pid>/status
│   ├── cmdline.rs   # /proc/<pid>/cmdline
│   ├── stat.rs      # /proc/<pid>/stat
│   ├── maps.rs      # /proc/<pid>/maps (预留)
│   └── fd.rs        # /proc/<pid>/fd/ (预留 - Phase 4)
```

## Phase 0: 核心基础设施 (1 module, ~200 行)

### 0.1 创建 ProcFS 模块骨架

**文件**: `os/src/fs/procfs/mod.rs`

实现以下结构体：

```rust
pub struct ProcFS {
    root_inode: Arc<LockedProcInode>,
    self_ref: Mutex<Weak<ProcFS>>,
}

pub struct LockedProcInode(pub Mutex<ProcInodeData>);

pub struct ProcInodeData {
    parent: Weak<LockedProcInode>,
    self_ref: Weak<LockedProcInode>,
    fs: Weak<ProcFS>,
    metadata: Metadata,
    /// 目录子项（目录 inode 使用）
    children: BTreeMap<String, Arc<dyn IndexNode>>,
    /// 内容生成函数（文件 inode 使用）
    content_fn: Option<ProcContentFn>,
}

/// 内容生成函数类型：offset、len、buf → 读取字节数
type ProcContentFn = fn(offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr>;
```

实现：
- `ProcFS::new()` — 创建文件系统实例，生成根目录 inode
- `ProcFS::new_with_entries()` 或 `add_entry()` — 向根目录注册文件/子目录
- `LockedProcInode::new_dir()` — 创建目录 inode
- `LockedProcInode::new_file()` — 创建文件 inode（带 content_fn）
- `LockedProcInode::insert_child()` — 添加子项

**IndexNode impl for LockedProcInode**:
- `read_at()` — 如果 content_fn 存在则调用，否则返回 EISDIR/ENOSYS
- `write_at()` — 返回 EPERM (procfs 只读)
- `find()` — 在 children map 中查找，支持 "." / ".."
- `list()` — 返回 children keys + "." + ".."
- `metadata()` — 返回存储的 metadata
- `fs()` — 返回 ProcFS 的 Arc
- `as_any_ref()` — 使用 `impl_index_node_as_any!` 宏
- `open()` / `close()` — 默认实现
- `resize()` — 空操作（允许 O_TRUNC）

**FileSystem impl for ProcFS**:
- `root_inode()` — 返回根 inode
- `info()` — 返回 FsInfo (blk_dev_id=0, max_name_len=255)
- `name()` — "proc"
- `super_block()` — PROC_SUPER_MAGIC (0x9fa0)
- `as_any_ref()` — self

### 0.2 注册 procfs 模块

**文件**: `os/src/fs/mod.rs`
- 添加 `pub mod procfs;`
- 在 `VFS_ROOT` 初始化中（ramfs 和磁盘 FS 两种路径），在 /dev 挂载之后添加 /proc 挂载：
  ```rust
  // 创建 /proc 目录并挂载 ProcFS
  let proc_inode = root.create("proc", FileType::Dir, InodeMode::from_bits_truncate(0o555))
      .expect("failed to create /proc");
  let proc_inode_id = proc_inode.metadata().expect("...").inode_id;
  let procfs = crate::fs::procfs::ProcFS::new();
  let procfs_mnt = MountFS::new(procfs, MountFlags::empty());
  mfs.add_mount(proc_inode_id, procfs_mnt).expect("failed to mount procfs at /proc");
  ```
- 同时在 ext4/fat32 分支也添加（两个分支都需要）

### 验收标准
- [ ] `make rv64-kernel-build-only` 编译通过
- [ ] `make la64-kernel-build-only` 编译通过
- [ ] 代码结构清晰，所有默认 trait 方法返回合理错误

**→ Oracle 审查 Phase 0 设计**

---

## Phase 1: 根目录静态文件 (4 files, ~250 行)

### 1.1 `/proc/version`

**文件**: `os/src/fs/procfs/files/version.rs`

```rust
fn version_content(offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
    let s = "OSKernel2026-Mango 0.1.0 (riscv64 / loongarch64)\n";
    proc_read_str(offset, len, buf, s)
}
```

### 1.2 `/proc/uptime`

**文件**: `os/src/fs/procfs/files/uptime.rs`

调用 `crate::timer::uptime()` → 格式化为 `"{uptime}.00 {idle}.00\n"`

### 1.3 `/proc/meminfo`

**文件**: `os/src/fs/procfs/files/meminfo.rs`

从以下来源动态生成：
- `unallocated_frames()` → MemFree
- `heap_stats()` → 内核堆统计
- `page_cache::cached_page_count()` → Cached
- `page_cache::dirty_count()` → Dirty
- 硬编码常量：MEMORY_SIZE → MemTotal

### 1.4 `/proc/cpuinfo`

**文件**: `os/src/fs/procfs/files/cpuinfo.rs`

动态生成：
- 架构字符串（通过 cfg!(target_arch = ...)）
- `get_clock_freq()` → BogoMIPS
- 简化：单核，硬编码基本字段

### 1.5 辅助函数

```rust
/// 通用 proc 文件读取：offset/len 边界检查 + 拷贝
fn proc_read_str(offset: usize, len: usize, buf: &mut [u8], data: &str) -> Result<usize, SyscallErr>
```

### 验收标准
- [ ] 4 个文件的构造函数注册到 ProcFS 根目录
- [ ] 双架构编译通过
- [ ] QEMU 启动后能 `cat /proc/version`、`cat /proc/meminfo` 等

**→ Oracle 审查 Phase 1**

---

## Phase 2: 进程相关 (3 files, ~350 行)

### 2.1 `/proc/self`

**文件**: `os/src/fs/procfs/files/self_.rs`

符号链接实现：content_fn 返回 `current_task().pid.to_string()` 作为链接目标。

### 2.2 `/proc/<pid>/` 目录

**文件**: `os/src/fs/procfs/pid/mod.rs`

`LockedProcInode::new_pid_dir(pid)` — 创建 PID 目录 inode。
- `find()` 中动态检查：若 name 为数字且进程存在 → 组装 `PidDirInode`
- `list()` 中遍历 `TASK_MANAGER` 的 ready_queue + interruptible_queue
- 使用 `find_task_by_pid()` 验证 PID 有效性

### 2.3 `/proc/<pid>/status`

**文件**: `os/src/fs/procfs/pid/status.rs`

从 `TaskControlBlock` 动态生成标准 Linux 格式：
```
Name:   initproc
State:  R (running)
Tgid:   1
Pid:    1
PPid:   0
Uid:    0   0   0   0
Gid:    0   0   0   0
FDSize: 256
VmSize:  xxx kB
VmRSS:   xxx kB
...
```

### 2.4 `/proc/<pid>/cmdline`

**文件**: `os/src/fs/procfs/pid/cmdline.rs`

从 `TaskControlBlock.exe_path` 生成（用 \0 分隔参数，当前简化为只有程序路径）。

### 验收标准
- [ ] `ls /proc` 显示数字 PID 目录
- [ ] `cat /proc/self/status` 显示当前进程状态
- [ ] `cat /proc/1/status` 显示 initproc 状态
- [ ] 双架构编译通过 + QEMU 测试

**→ Oracle 审查 Phase 2**

---

## Phase 3: 系统级文件 (2 files, ~300 行)

### 3.1 `/proc/mounts`

**文件**: `os/src/fs/procfs/files/mounts.rs`

遍历 `VFS_ROOT` 的 mountpoints 表生成内容（格式: `fsname dir type opts freq passno`）。
若 mountpoints API 不足以遍历，可先提供简化版（只显示根挂载点）。

### 3.2 `/proc/stat`

**文件**: `os/src/fs/procfs/files/stat.rs`

包含：
- `cpu  user nice system idle ...` 行（简化为全 0）
- `intr ... ctxt ...` 
- `btime {boot_time}`
- `processes {procs_count()}`
- `procs_running {ready_count}`
- `procs_blocked {interruptible_count}`

### 验收标准
- [ ] `cat /proc/mounts` 输出合理
- [ ] `cat /proc/stat` 输出标准 Linux 格式
- [ ] 双架构编译 + 测试

**→ Oracle 审查 Phase 3**

---

## Phase 4: 扩展与收尾 (预留，~200 行)

### 4.1 `/proc/<pid>/fd/`
遍历 `TaskControlBlock.files`（FdTable），为每个 fd 创建符号链接 inode。

### 4.2 `/proc/<pid>/maps`
遍历 `AddressSpace.vmas` 生成内存映射表。

### 4.3 其他预留
- `/proc/net/tcp` — TCP socket 列表
- `/proc/loadavg` — 负载平均值

### 验收标准
- [ ] 根据实际需要选择性实现

**→ Oracle 审查 Phase 4**

---

## 总量估算

| Phase | 新文件数 | 代码行数 | 编译难度 | 测试复杂度 |
|-------|---------|---------|---------|-----------|
| 0     | 1 mod   | ~200    | 低      | 编译即可   |
| 1     | 5 files | ~250    | 低      | QEMU cat  |
| 2     | 4 files | ~350    | 中      | PID 遍历  |
| 3     | 2 files | ~300    | 中      | 格式验证  |
| 4     | 2-4 files | ~200  | 中      | 按需       |
| **总计** | **~14 files** | **~1300** | | |

## 高风险点

1. **PID 目录动态创建时的并发安全** — `find()` 和 `list()` 需要持有锁时遍历 TASK_MANAGER。解决方案：使用独立的 PID → inode 缓存，避免在锁内创建新 Arc。

2. **ext4/fat32 分支也需要挂载 procfs** — fs/mod.rs 有多个分支，每个都需要添加 /proc 挂载，避免只在一个分支处理。

3. **符号链接的 /proc/self** — 需要确保 vfs_lookup 的符号链接跟随机制与 procfs 的 content_fn 协同工作。当 content_fn 返回的内容被解释为路径时，vfs_lookup 需要正确处理。

4. **双架构编译差异** — procfs 代码应保持架构无关，使用 `cfg!()` 或 HAL 抽象层获取架构特定信息。

5. **内存分配** — procfs 的 `content_fn` 会大量使用 `String` 动态分配。需确保不在中断上下文调用，不使用可能 OOM 的分配。

## 验证清单

- [ ] Phase 0: `make rv64-kernel-build-only` + `make la64-kernel-build-only` 通过
- [ ] Phase 1: QEMU 启动后手动 `cat` 验证
- [ ] Phase 2: QEMU 启动后 `ls -la /proc`、`cat /proc/self/status` 验证
- [ ] Phase 3: QEMU 启动后 `cat /proc/mounts`、`cat /proc/stat` 验证
- [ ] 最终: `make all` 全量编译通过
- [ ] 最终: QEMU 启动不 panic，basic 测试组通过
