# MangoCore Mount 子系统实现计划

> 创建: 2026-05-22 | 更新: 2026-05-25
> Oracle 审查: 已通过 (2026-05-22, 2026-05-25)
> 分支: `fs`
> 配套文档: `Doc/ltp_mount_status.md`

## 1. 动机

### 1.1 现状（更新于 2026-05-25）

`sys_mount(40)` 和 `sys_umount2(39)` 已实现，MountFS/MountFSInode/mountpoints 基础设施完备。**Flag 路由已实现**（propagation → MS_BIND → MS_MOVE/MS_REMOUNT → 正常挂载），`do_bind_mount()` 初版已完成。

**当前待修复**:
- `do_recursive_bind` backref 指向 source 而非 target 侧（代码注释自认）
- `remove_mount()` 静默吞掉 `None`，umount 断环可观测性差
- `on_umount()` 不清 ext4 缓存
- `/proc/mounts` 只列根直接子挂载，硬编码路径名 → LTP cleanup 依赖此文件
- MountList 定义但未全局使用
- MS_REC 递归 bind 返回 EINVAL（初版 stub 存在）

**已修复**:
- ✅ `overlaid_inode()` 不再覆盖 `self_mountpoint`（对齐 DragonOS 语义）
- ✅ ext4 `children` 改为 `Weak<dyn IndexNode>`，不再泄漏
- ✅ 用户确认内存泄漏问题已修

### 1.2 为什么现在做

MountFS 基础设施已就绪。Bug 修复量小（~50 行），完成后直接解锁 65 个 fs_bind 测例（bind/ + rbind/）→ 这是当前性价比最高的 LTP 增量。

### 1.3 范围边界

| 做 | 不做 |
|----|------|
| ✅ remove_mount / umount 断环修复 | ❌ mount propagation (shared/slave/private/unbindable) |
| ✅ on_umount cache eviction | ❌ mount namespace (CLONE_NEWNS) |
| ✅ `MountFSInode::mount_subtree()` | ❌ MS_MOVE |
| ✅ MS_BIND 基础 bind mount（含 self-bind） | ❌ /dev/loop* 管理 |
| ✅ MS_REC 递归 bind（快照+回滚） | ❌ 新 mount API (fsopen/fsmount/open_tree/move_mount) |
| ✅ `/proc/mounts` 递归枚举 | ❌ 非 mount 的 FS 修复 |
| ✅ MountList 全局同步 | |
| ✅ 递归 bind 防环 | |

---

## 2. 分阶段计划

### Phase 0: 文档体系建设 ✅
**已完成: 2026-05-25**

- ✅ 创建 `Doc/ltp_mount_status.md` — 94 测例状态表
- ✅ 更新 `Doc/ltp_mount_plan.md` — 反映当前实现状态
- 后续: 记录内存基准指标

---

### Phase 1: 止血 — 修复 umount / 挂载生命周期 bug
**预计: 0.5 天 | 参考: DragonOS mount.rs**

#### 1.1 `remove_mount()` 吞 None 静默

**文件**: `os/src/fs/vfs/mount.rs:568-581`

当前 `umount()` 中:
```rust
if let Some(mountpoint) = self.self_mountpoint.lock().take() {
    if let Ok(md) = mountpoint.inner_inode.metadata() {
        mountpoint.mount_fs.remove_mount(md.inode_id);
    }
}
```

`remove_mount()` 返回 `None` 时完全吞掉。应至少在计数器/log 可观测，或对关键场景返回错误。

#### 1.2 `on_umount()` 加 cache eviction

**文件**: `os/src/fs/ext4/ext4fs.rs`

当前 `on_umount()` 只做 flush（line 2173-2176），不清 dentry/weak registry。需要:
- 调用 `clear_all_children_caches()` 清理 stale Weak 条目
- `prune_stale_weak_entries()` 回收过期 Weak

#### 1.3 修复 `do_recursive_bind` backref

**文件**: `os/src/syscall/fs.rs:2216-2223`

当前 backref 指向 source 侧 inode。代码注释自述: "Actually we need the target's MountFSInode, not source"。改为指向 target 侧。

#### 验收标准

- [ ] 连续 N 次 mount/umount 后 `MOUNTFS_ALIVE`/`MOUNTFSINODE_ALIVE` 回到基线
- [ ] `heap_free` 不持续单调下降
- [ ] `make rv64-kernel-build-only` + `make la64-kernel-build-only` 通过
- [ ] 现有 fs 回归集无回退

---

### Phase 2: MS_BIND — 引入 `mount_subtree()` + self-bind + `/proc/mounts`
**预计: 1 天 | 覆盖: fs_bind/bind/ 25 脚本 | 参考: DragonOS sys_mount.rs:477**

#### 2.1 新增 `MountFSInode::mount_subtree()`

**文件**: `os/src/fs/vfs/mount.rs`

```rust
impl MountFSInode {
    pub fn mount_subtree(
        &self,                           // target-side inode
        inner_fs: Arc<dyn FileSystem>,
        root_inner_inode: Arc<dyn IndexNode>,
        mount_flags: MountFlags,
    ) -> Result<Arc<MountFS>, SyscallErr>
}
```

封装: `MountFS::new_with_root()` → `add_mount()` → `set_self_mountpoint(target)` → 失败回滚。

参考 DragonOS `mount.rs:477-533`，裁剪掉 propagation 和 namespace 逻辑。

#### 2.2 重写 `do_bind_mount`

**文件**: `os/src/syscall/fs.rs:2108-2172`

改为:
```rust
fn do_bind_mount(...) -> isize {
    // 1. lookup source
    // 2. 获取 source_inner_fs + source_inner_inode
    // 3. target_mfs_inode.mount_subtree(source_inner_fs, source_inner_inode, flags)
    // 4. 设置 mount_source
    // 5. 如 MS_REC → do_recursive_bind_mount (Phase 3)
}
```

**关键**: source 和 target 共享 `inner_filesystem()`，不创建新 FS 实例。

#### 2.3 支持 self-bind

`mount --bind dir dir` **必须允许**。这是 LTP `fs_bind_lib.sh` 的 `fs_bind_makedir()` 前置条件。Linux 语义允许 self-bind。

实现: 在 `mount_subtree()` 中不拒绝 target==source 的情况（与 DragonOS 对齐）。

**但需注意**: self-bind 后 `overlaid_inode` 可能产生循环；确保 `mountpoint_root_inode()` 的 overlay 逻辑能正确处理 self-bind 场景。

#### 2.4 修复 `/proc/mounts` 递归枚举

**文件**: `os/src/fs/procfs/files/mounts.rs`

当前只列根的直接子挂载，硬编码路径名。LTP cleanup (`fs_bind_lib.sh`) 依赖 `/proc/mounts` 反向卸载 sandbox mount。

改后: 递归遍历 MountFS.mountpoints 树，输出完整挂载列表，包含实际路径和 mount_source。

#### 2.5 MountList 全局同步

**文件**: `os/src/fs/vfs/mount.rs` + `os/src/fs/procfs/files/mounts.rs`

mount/umount 时同步更新全局 MountList（当前已定义但未使用）。用于 `/proc/mounts` 和 path-based lookup。

#### 验收标准

- [ ] `mount --bind /tmp/a /tmp/b` 成功，写入共享
- [ ] `mount --bind dir dir` (self-bind) 成功
- [ ] `umount /tmp/b` 后父 mountpoints 表中无残留
- [ ] `/proc/mounts` 可见 bind mount 条目
- [ ] 连续 bind/umount 后 alive counters 稳定
- [ ] 回归集无回退
- [ ] fs_bind/bind/ 子集开始 PASS

---

### Phase 3: MS_REC — 递归 bind mount
**预计: 0.5-1 天 | 覆盖: fs_bind/rbind/ 40 脚本 | 参考: DragonOS sys_mount.rs:515**

#### 3.1 快照-优先策略

**核心原则**: 先快照 source mount tree，再创建 target mount。避免 BFS 遍历时把刚创建的新目标纳入。

```rust
fn do_recursive_bind_mount(source_mfs, target_mfs, source_base, target_base) {
    // Step 1: 快照 source 下所有 submounts → Vec<(path, child_mfs)>
    // Step 2: 对每个 snapshot 条目:
    //   - 计算 relative_path
    //   - lookup target 对应路径
    //   - mount_subtree()
    // Step 3: 任一失败 → 逆序回滚已创建 mount
}
```

参考 DragonOS `do_recursive_bind_mount()` (sys_mount.rs:515-653)，裁剪 MountList.get_mount_path_by_ino 为直接遍历 mountpoints 树。

#### 3.2 all-or-nothing 回滚

维护 `created: Vec<Arc<MountFS>>`，失败时逆序调用每个已创建 MountFS 的 `umount()`。

不能用普通 `umount()` 从根开始，因为可能遇到带子挂载的中间节点 → EBUSY。

#### 3.3 防环 + `seen_set`

- `seen_set`: `HashSet<(MountFS_addr, InodeId)>` 防重复处理
- `is_ancestor_or_same()`: 跨 MountFS 比较，防挂载环

#### 验收标准

- [ ] `mount --rbind /tmp/sandbox /tmp/copy` 子挂载递归复制
- [ ] 递归 bind 不产生挂载环
- [ ] rbind 失败场景下已创建 mount 全部回滚
- [ ] fs_bind/rbind/ 子集开始 PASS
- [ ] 反复 rbind/umount 后 alive counters 稳定

---

## 3. 风险清单

### 3.1 高风险

| 风险 | 影响 | 缓解 |
|------|------|------|
| **self-bind 产生 overlay 循环** | `mount --bind dir dir` 后 `overlaid_inode` 可能无限递归 | 验证 `mountpoint_root_inode()` 的 overlay 逻辑能正确处理 self-bind；必要时加 seen_set |
| **rbind 回滚不完整** | 残留孤儿 MountFS → 后续 umount/lookup 随机失败 | 维护 created list，逆序 detach；不依赖普通 umount |
| **递归 bind 环** | mount tree 形成环 → `vfs_lookup` 无限循环 → kernel panic | `seen_set` 防重复 + `is_ancestor_or_same()` 检查 |

### 3.2 中风险

| 风险 | 影响 | 缓解 |
|------|------|------|
| **过度清 cache** | 共享 ext4 FS 的 bind mount 性能回退 | 只在 `on_umount()` 做 eviction，不逐 mount 清理 |
| **错误码偏差** | 与 Linux 语义不一致导致 LTP TFAIL | 每步对照 Linux 6.6 语义验证 |
| **/proc/mounts 格式** | LTP cleanup 依赖 `/proc/mounts` 解析 | 对齐 Linux mountinfo 格式，提供完整挂载树 |

### 3.3 低风险

| 风险 | 影响 | 缓解 |
|------|------|------|
| **file bind mount** | bind 到非目录语义不同 | Phase 2 只支持目录 bind，状态文档标注 |
| **重复挂载点** | 同一目录多次挂载 | 允许叠加（Linux 语义），`add_mount` 改 `insert` 替换 |

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
Phase 0: 文档体系 ────────────────────────────────────────────► 已完成
  ├─ ✅ 创建 ltp_mount_status.md
  └─ ✅ 更新 ltp_mount_plan.md

Phase 1: 止血 ───────────────────────────────────────────────► 0.5 天
  ├─ 1.1 remove_mount() 吞 None 可观测化
  ├─ 1.2 on_umount() cache eviction
  ├─ 1.3 do_recursive_bind backref 修复
  └─ 验收: alive counters + 回归

Phase 2: MS_BIND ────────────────────────────────────────────► 1 天
  ├─ 2.1 mount_subtree() 新方法
  ├─ 2.2 do_bind_mount 重写
  ├─ 2.3 self-bind 支持
  ├─ 2.4 /proc/mounts 递归枚举
  ├─ 2.5 MountList 全局同步
  └─ 验收: bind 子集 PASS + counters 稳定

Phase 3: MS_REC ─────────────────────────────────────────────► 0.5-1 天
  ├─ 3.1 快照-优先 BFS
  ├─ 3.2 all-or-nothing 回滚
  ├─ 3.3 防环 + seen_set
  └─ 验收: rbind 子集 PASS

Phase 4: LTP 验证 ───────────────────────────────────────────► 0.5 天
  ├─ 分批发现扫
  ├─ include 列表扩展
  └─ 更新状态文档

后续（独立专项，不混入本轮）:
  ├─ mount propagation (shared/slave/private/unbindable)
  ├─ MS_MOVE
  ├─ mount namespace (CLONE_NEWNS)
  ├─ /dev/loop* + block device hotplug
  └─ 新 mount API (fsopen/fsmount/open_tree/move_mount)
```

## 6. Oracle 审查记录

| 日期 | 结论 |
|------|------|
| 2026-05-22 | ✅ 通过。建议: 只做 MS_BIND + MS_REC，不做传播/namespace/loopback。先修 filesystemtype=NULL 入口校验。参考 DragonOS 结构设计但不移植传播引擎。 |
| 2026-05-25 | ✅ 通过。关键调整: (1) Phase 0 先验证当前基线（overlaid_inode/ext4 children 可能已修复），(2) Phase 1 只做最小修复，(3) 新增 self-bind 支持 + /proc/mounts 修复，(4) rbind 快照-优先+回滚, (5) 验收标准加 alive counters/heap_free/mountpoints 清理指标，(6) 不做 MS_MOVE。 |

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
