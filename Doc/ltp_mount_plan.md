# MangoCore Mount 子系统实现计划

> 创建: 2026-05-22
> Oracle 审查: 已通过 (2026-05-22)
> 分支: `fs` (后续合并到 `develop`)

## 1. 动机

### 1.1 现状

`sys_mount(40)` 和 `sys_umount2(39)` 已实现，MountFS/MountFSInode/mountpoints 基础设施完备，但：

- `sys_mount` **无条件创建新 RamFS 挂载**，完全不检查 MS_BIND/MS_REC/MS_MOVE
- 第 2114 行 `filesystemtype.is_null() → EINVAL` 直接拦截所有 bind mount 请求

```
mount --bind sandbox sandbox          → EINVAL (filesystemtype=NULL)
mount --rbind parent1 share1         → EINVAL (同上)
mount --make-shared target           → 无逻辑
```

### 1.2 为什么现在做

| 剩余"小修" | 实际依赖 | 性价比 |
|-----------|---------|--------|
| EACCES x8 | seteuid→完整权限模型 | 低，不是 FS 单模块 |
| open08 | FIFO 阻塞→调度基础设施 | 低 |
| mkdir04 | VFS 路径→未知调试深度 | 中 |
| getsockopt01 1 TFAIL | 真边角 | 高（顺手清） |

而 mount 的 **MS_BIND + MS_REC** 是中等风险、测试收益最高（65 脚本）的下一档——MountFS 基础设施已就绪，不是从零造轮子。

### 1.3 范围边界

| 做 | 不做 |
|----|------|
| ✅ MS_BIND 基础 bind mount | ❌ mount propagation (shared/slave/private/unbindable) |
| ✅ MS_REC 递归 bind | ❌ mount_namespace (CLONE_NEWNS) |
| ✅ MS_MOVE (可选 P3) | ❌ /dev/loop* 管理 |
| ✅ 路径解析跨 mountpoint 正确性 | ❌ 新 mount API (fsopen/fsmount/open_tree/move_mount) |
| ✅ 递归 bind 防环 | ❌ 非 mount 的 FS 修复（不含 EACCES/open08 等） |

---

## 2. 分阶段计划

### Phase 1: MS_BIND — 基础 bind mount
**预计: 1-2 天 | 覆盖: fs_bind/bind/ 25 脚本**

#### 2.1.1 修复 sys_mount 入口参数校验

**文件**: `os/src/syscall/fs.rs:2114`

当前：
```rust
if target.is_null() || filesystemtype.is_null() {
    return EINVAL;
}
```

改后逻辑：
```rust
// MS_BIND / MS_MOVE 路径下允许 filesystemtype=NULL
// 但 target 绝对不能为 NULL
if target.is_null() {
    return EINVAL;
}
// 先解析 mountflags 再决定 filesystemtype 是否必须
let mountflags = match MountFlags::from_bits(mountflags_raw) { ... };
let needs_fstype = !mountflags.intersects(MountFlags::MS_BIND | MountFlags::MS_MOVE);
if needs_fstype && filesystemtype.is_null() {
    return EINVAL;
}
```

> **注意**: `mountflags` 解析需要移到 `filesystemtype` 校验之前，改变现有代码流。

#### 2.1.2 实现 bind mount 逻辑

**文件**: `os/src/syscall/fs.rs:2143-2202`（现有 sys_mount 尾部）

在现有"创建新 RamFS"分支之前插入 MS_BIND 检测：

```rust
if mountflags.contains(MountFlags::MS_BIND) {
    // 1. 解析 source 路径 → source inode
    // 2. 检查 target 目录存在
    // 3. 创建 bind mount：source inode 的 MountFS 作为新挂载点的文件系统
    // 4. 注册到 parent_mount_fs.add_mount()
    return do_bind_mount(source_path, target_inode, mountflags);
}
```

#### 2.1.3 do_bind_mount 核心逻辑

```
do_bind_mount(source_path, target_inode, mountflags):
  1. vfs_lookup(source_path) → source_inode
  2. 从 source_inode 获取其 MountFS（通过 downcast MountFSInode）
  3. 检查 source 不是 target 的祖先（防环）
  4. 如果 mountflags 包含 MS_REC → Phase 2
  5. 创建新的 MountFS 或复用 source 的 MountFS
     - 策略: source_inode 作为新挂载的 root_inner_inode
     - MountFS 标记 mount_source = source_path
     - MountFlags 从 mountflags 转换
  6. target 父 MountFS.add_mount(target_inode_id, new_mount_fs) → 注册
  7. 更新 /proc/mounts（MountList 已存在，可复用）
  8. 返回 SUCCESS
```

**关键决策**: 不创建新的 FileSystem 实例。bind mount 是同一 inode 子树的不同视图，共享 page cache 和 inode 状态。

#### 2.1.4 需要改动/新增的文件

| 文件 | 改动 |
|------|------|
| `os/src/syscall/fs.rs` | 参数分流 + `do_bind_mount()` + `do_recursive_bind()` |
| `os/src/fs/vfs/mount.rs` | `MountFS::new_bind(source_inode, flags)` 构造函数 |
| `os/src/fs/mod.rs` | 可能需要导出辅助函数 |
| `os/src/fs/procfs/files/mounts.rs` | bind mount 条目显示（mount_source 字段已有） |

#### 2.1.5 Phase 1 验收

- [ ] `mount --bind /tmp/a /tmp/b` 成功，`ls /tmp/b` 显示与 `/tmp/a` 相同内容
- [ ] `mount --bind /tmp/a /tmp/b` 后向 `/tmp/b/x` 写入，`/tmp/a/x` 可见（共享语义）
- [ ] `/proc/mounts` 显示 bind mount 条目
- [ ] `umount /tmp/b` 成功
- [ ] 静态 mount（/dev、/proc、/tmp）无回归
- [ ] `make rv64-kernel-build-only` + `make la64-kernel-build-only` 通过
- [ ] fs_bind/bind/ 子集基本通过（至少 bind01-10）
- [ ] open07/link04/unlink08 等已验证 PASS 测例无回退

---

### Phase 2: MS_REC — 递归 bind mount
**预计: +0.5 天 | 覆盖: fs_bind/rbind/ 40 脚本**

#### 2.2.1 递归策略

当 `mountflags.contains(MountFlags::MS_REC)`：

```
do_recursive_bind(source_inode, target_parent, mountflags):
  1. 对 source_inode 做 bind mount（Phase 1 逻辑）
  2. 遍历 source MountFS 的 mountpoints 表
     - 对每个 (child_inode_id, child_mount_fs)：
       - 在 target 下找到对应 child_inode
       - 递归 bind mount child 的整个 MountFS
  3. 注意：只复制挂载结构，不实现 shared/slave/private 传播语义
```

#### 2.2.2 防环检查（Phase 1 也需要）

```rust
fn is_ancestor_or_same(ancestor: &Arc<dyn IndexNode>, descendant: &Arc<dyn IndexNode>) -> bool {
    // 通过比较 inode_id 和 MountFS 实例判断
    // 最简单：拒绝 source == target 或 source 是 target 祖先
}
```

递归 bind 时需要一个 `seen_set` 防无限循环。

#### 2.2.3 Phase 2 验收

- [ ] `mount --rbind /tmp/a /tmp/b` 成功，子挂载点被递归复制
- [ ] 递归 bind 不产生挂载环
- [ ] fs_bind/rbind/ 子集基本通过
- [ ] 无内存泄漏（反复 bind/umount 后内存稳定）

---

### Phase 3: MS_MOVE — 移动挂载点（可选）
**预计: +0.5 天 | 覆盖: fs_bind/move/ 22 脚本**

MS_MOVE 语义：原子地将挂载点从 source 移到 target。

```
do_move_mount(source_path, target_path):
  1. vfs_lookup(source) → source_mountpoint (必须是挂载点根)
  2. vfs_lookup(target) → target_parent (必须是目录、非挂载点)
  3. 从旧父 MountFS 的 mountpoints 表删除
  4. 更新 MountFSInode 的 self_mountpoint
  5. 注册到新父 MountFS 的 mountpoints 表
  6. 防环检查
```

> **当前不优先**：Phase 3 是加分项。如果 bind/rbind 稳定且时间允许再做。move 22 脚本的优先级低于 bind 25 + rbind 40。

---

## 3. 风险清单

### 3.1 高风险

| 风险 | 影响 | 缓解 |
|------|------|------|
| **跨 mountpoint 路径解析** | bind 后 `vfs_lookup` 穿过新挂载边界可能出错，导致所有路径操作回归 | 每步改完立刻跑 open/read/write/stat 回归集 |
| **递归 bind 环** | mount tree 形成环 → `vfs_lookup` 无限循环 → kernel panic | Phase 1 先拒绝所有危险组合（same/ancient），Phase 2 加 seen_set |
| **page cache 一致性** | 如果 bind mount 克隆了 page cache → 两个视图数据不一致 | 设计上 bind mount 不创建新 FileSystem，共享同一实例 |

### 3.2 中风险

| 风险 | 影响 | 缓解 |
|------|------|------|
| **umount 语义** | bind mount 被卸载时，原挂载点应保持独立 | 验证 umount 后只有 bind mount 消失，原 source 不受影响 |
| **MountFS 引用计数** | 如果在 bind mount 的 source 卸载时释放了共享的 inode | 依赖 Arc 引用计数，仅在 mountpoints 表中删除条目 |
| **/proc/mounts 格式** | bind mount 的显示格式必须给 shell 脚本可解析 | 参考 Linux `/proc/self/mountinfo` 格式 |

### 3.3 低风险

| 风险 | 影响 | 缓解 |
|------|------|------|
| **bind 到文件** | 如果 bind 目标不是目录而是文件，语义不同 | Phase 1 只支持目录 bind（LTP bind 测试先用目录） |
| **MS_RDONLY 叠加** | bind mount 可能携带只读 flag | 逐位处理 MountFlags 转换 |

---

## 4. 回归测试策略

### 每次修改后必须跑

```
1. make rv64-kernel-build-only    ← 编译
2. make la64-kernel-build-only    ← 双架构
3. QEMU 回归集 (至少):
   - open03, open04, open07
   - close01, close02
   - read01, read02, write01, write02
   - lseek01, stat02, fstat02
   - getcwd01, chdir04, getdents01
   - dup01-07, dup201-207, dup3_01
   - fcntl01-05, fcntl08, fcntl13-14
   - unlink05/07/08, link02/04/05
   - readlink01/03, ftruncate01
   - pipe2_01
   - iperf/netperf (确认网络栈无回归)
4. fs_bind/bind/ 当前阶段目标子集
```

### 完成 Phase 1/2 后跑

```
mask=0xFFF 全量 LTP（确认无回归 panic）
```

---

## 5. 实现顺序总览

```
Phase 1: MS_BIND ─────────────────────────────────────────► 1-2 天
  ├─ 1.1 修复 sys_mount 参数校验 (filesystemtype=NULL)
  ├─ 1.2 实现 do_bind_mount()
  ├─ 1.3 MountFS 支持 bind 模式
  ├─ 1.4 防环检查
  └─ 1.5 验证: 回归集 + bind 子集

Phase 2: MS_REC ──────────────────────────────────────────► +0.5 天
  ├─ 2.1 do_recursive_bind()
  ├─ 2.2 mountpoints 表递归复制
  └─ 2.3 验证: 回归集 + rbind 子集

Phase 3: MS_MOVE (可选) ───────────────────────────────────► +0.5 天
  ├─ 3.1 do_move_mount()
  └─ 3.2 验证: 回归集 + move 子集

后续（独立专项，不混入本轮）:
  ├─ mount propagation (shared/slave/private/unbindable)
  ├─ mount namespace (CLONE_NEWNS)
  ├─ /dev/loop* + block device hotplug
  └─ 新 mount API (fsopen/fsmount/open_tree/move_mount)
```

---

## 6. Oracle 审查记录

| 日期 | 结论 |
|------|------|
| 2026-05-22 | ✅ 通过。建议: 只做 MS_BIND + MS_REC，不做传播/namespace/loopback。先修 filesystemtype=NULL 入口校验。参考 DragonOS 结构设计但不移植传播引擎。 |

---

## 7. 参考资料

| 来源 | 文件 | 用途 |
|------|------|------|
| 本内核 | `os/src/syscall/fs.rs:2107-2202` | sys_mount 当前实现 |
| 本内核 | `os/src/fs/vfs/mount.rs` | MountFS/MountFSInode 基础设施 |
| 本内核 | `os/src/fs/mod.rs:48-160` | VFS_ROOT 初始化 + mount_common_filesystems |
| DragonOS | `kernel/src/filesystem/vfs/mount.rs` | MountFS 结构参考 |
| Linux | `fs/namespace.c` (6.6) | bind mount 语义参考 |
| LTP | `testcases/kernel/fs/fs_bind/` | 测试脚本源码 |
