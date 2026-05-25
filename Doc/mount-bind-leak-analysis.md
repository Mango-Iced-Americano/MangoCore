# Mount Bind 内存泄漏分析报告

> 创建: 2026-05-23
> 分析范围: `fs` 分支 `ded1c0c`（MS_BIND 提交）的双架构 LTP 测试日志
> 日志来源: `testresult/output-rv.txt`, `testresult/output-la.txt`

## 1. 现象

### rv64（riscv64）

| 指标 | bind 测试开始 | bind 测试结束 | 泄漏量 |
|------|:---:|:---:|:---:|
| heap_free | ~259,700K | ~2,100K | **~257MB** |
| free_frames | ~193,000 | ~93,700 | ~397MB 物理帧 |

- 跑 24 个 rbind 脚本（rbind01-24），每个脚本泄漏 ~10MB heap
- 每个 `umount` TFAIL 后 heap_free 下降 37-63K
- 最终 `fs_bind_rbind24.sh` 测试中 `execve` 触发 `HEAP ALLOCATION FAILED (FATAL)`

### la64（loongarch64）

| 指标 | bind 测试开始 | 最终 | 泄漏量 |
|------|:---:|:---:|:---:|
| heap_free | ~231,677K | ~70,603K | **~161MB** |
| free_frames | ~139,019 | ~66,079 | **~285MB**（72,940帧） |

- 跑 85 个 bind 脚本（bind01-24 + rbind01-39 + move01-22）
- 物理帧泄漏比 heap 泄漏更严重
- 最终 fallocate01 时 `AddressError` panic（`bad_addr=0x1200c9e14`），疑似内存耗尽后返回无效地址

## 2. 根因分析（三个层次）

### 层次 1：ext4 `children` 强引用缓存 — 主犯

**位置**：`os/src/fs/ext4/layout.rs:93`
```rust
pub(super) children: Mutex<BTreeMap<String, Arc<dyn IndexNode>>>
```

每个目录 inode 的 `children` 是**强 `Arc` 引用表**，无 LRU 驱逐、无大小限制。一旦 `find()` 查过一个子目录/文件，那个 `Arc` 就永远被缓存。

**泄漏因果链**：
1. bind mount 创建新 `MountFS`，共享同一个 `Ext4FileSystem`
2. 路径查找触发 `Ext4OSInode::find()` → `children.insert(name, child_inode.clone())`
3. Umount 调用 `on_umount()`（`ext4fs.rs:1939`），但**只做 flush，不清 `children`**
4. `children` 中的强 `Arc` 阻止整个 inode 子树释放 → 每个 inode 的 page cache、metadata cache 锁定的物理帧也无法回收

**关键事实**：`clear_all_children_caches()` 已经写好（`ext4fs.rs:1507`），但**只在 debug syscall（counter 9）中暴露**，umount 路径从未调用。

### 层次 2：MountFS 引用环 — 从犯

**位置**：`os/src/fs/vfs/mount.rs`

```
子 MountFS.self_mountpoint (Arc<MountFSInode>)
  → MountFSInode.mount_fs (Arc<MountFS>)
  → 父 MountFS

父 MountFS.mountpoints (BTreeMap<InodeId, Arc<MountFS>>)
  → 子 MountFS
```

正常情况下 `MountFS::umount()` 用 `.take()` 清除 `self_mountpoint` 并 `remove_mount()` 清除父的 `mountpoints` 条目，断开环。但存在三个问题：

- **`overlaid_inode()`**（`mount.rs:170`）在**每次路径查找穿过挂载点时**都覆盖 `self_mountpoint`。DragonOS 是 mount 创建时设定 backref，lookup 不应改变生命周期元数据
- **`do_recursive_bind` 的 `self_mountpoint` 指向错误父 MountFS**（`syscall/fs.rs:2216`，注释自述："Actually we need the target's MountFSInode, not source"）
- **MS_REC 直接返回 `EINVAL`**，测试脚本继续跑其他 mount/umount 操作，部分 mount 创建成功但引用环未正确建立，后续 umount 找不到正确父节点 → 返回 `EINVAL` / `Resource busy`

### 层次 3：`/tmp` 目录残留 — 不是真泄漏

`/tmp` 下的残留文件和目录占用 ext4 磁盘块，不是 kernel heap。只有被 `children` cache 或 page cache 强引用时才表现为 heap 增长。

## 3. DragonOS 对照

| 设计点 | DragonOS | MangoCore 当前 | 缺口 |
|--------|----------|:---:|------|
| dentry cache | **不存在**。只有 `DName(Arc<String>)` | **存在**：ext4 `children: BTreeMap<String, Arc<dyn IndexNode>>` | 强引用缓存是泄漏主因 |
| MountFSInode.self_ref | `Weak` (`Arc::new_cyclic`) | `Weak` (`Arc::new_cyclic`) | ✅ 一致 |
| self_mountpoint 管理 | mount 创建时 set，**不在 lookup 时覆盖** | `overlaid_inode()` **每次 lookup 都覆盖** | ❌ 多余分配 |
| umount 环断开 | `do_umount()` remove + `self_mountpoint.take()` + `clear_namespace()` | `self_mountpoint.take()` + `remove_mount()` | 部分正确，但 backref 可能已被覆盖 |
| 递归 bind 回滚 | `umount_tree()` DFS 逆序，all-or-nothing | 无 | ❌ 部分 mount 成功留下孤儿 |
| on_umount 缓存清理 | 发 FS 特定清理 | **只 flush，不 evict children** | ❌ 主因 |

## 4. Oracle 修复建议

### 优先级 P0 — 止血

1. **`on_umount()` 加 `clear_all_children_caches()` + `prune_stale_weak_entries()`**
   - 文件：`os/src/fs/ext4/ext4fs.rs:1939`
   - 一行调用，直接释放 children 强引用链
   - 预计回收 60-80% 泄漏

2. **修复 `umount` detach 语义**
   - `remove_mount()` 失败不能吞掉，要返回错误
   - `overlaid_inode()` 不要在 lookup 时覆盖 `self_mountpoint`（参考 DragonOS 只在 mount 创建时设定）

### 优先级 P1 — 治本

3. **MS_REC 实现 all-or-nothing 回滚**（参考 DragonOS `umount_tree`）
   - 递归 bind 失败时不留孤儿 MountFS

4. **`children` 从 `Arc` 改为 `Weak`（或加 LRU 驱逐）**
   - 从根源消除无界缓存
   - 代码注释已标注"想用 Weak"但实际是强引用（`layout.rs:86-93`）

### 验证方案

- 用 ext4 debug ioctl 手动触发 `clear_all_children_caches`，若 `heap_free` 明显回升则坐实 dentry cache 是主因
- 每次修复后双架构编译 + QEMU 启动验证
- 跑 mount bind 测试，对比 umount 前后 `heap_free`

## 5. la64 AddressError 说明

`bad_addr=0x1200c9e14`，不直接归因于 MS_BIND。更可能是 `execve` 路径中使用了被前序测试破坏的指针，或物理帧泄漏导致内存分配返回无效地址。`Arc` 语义下被释放后又访问的概率极低。建议用 `addr2line` 定位 `pc=0x8015fc20` 后再独立判断。
