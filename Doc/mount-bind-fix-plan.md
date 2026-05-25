# Mount Bind 内存泄漏修复计划

> 创建: 2026-05-23
> 分支: `fix/mount-bind-leak`
> 审查: 每阶段完成后 Oracle 审查 → 人工审查

## 总览

| Phase | 目标 | 文件 | 预计 |
|:---:|------|------|:--:|
| 1 | 修复 `overlaid_inode` 覆盖 `self_mountpoint` | `mount.rs` | 小 |
| 2 | 修复 `do_recursive_bind` backref 指向错误 | `syscall/fs.rs` | 小 |
| 3 | `children` Arc → Weak + stale prune | `layout.rs`, `ext4fs.rs` | 中 |

## Phase 1: 修复 `overlaid_inode` 不覆盖 `self_mountpoint`

**问题**: `overlaid_inode()`（`mount.rs:160-176`）每次路径查找穿过挂载点时都调用 `sub.set_self_mountpoint(Some(self_inode))`，覆盖 mount 创建时设置的 backref。

**DragonOS 做法**: `self_mountpoint` 只在 mount 创建时设置，lookup 路径不修改。

**方案**: 移除 `overlaid_inode` 中的 `set_self_mountpoint` 调用。验证 umount 仍能正确找到父 MountFS 并成功 detach。

**验收**: 
- 双架构编译通过
- QEMU 启动不 panic
- umount 成功率提升（TFAIL 减少）

## Phase 2: 修复 `do_recursive_bind` backref

**问题**: `do_recursive_bind()`（`syscall/fs.rs:2215-2223`）将 `self_mountpoint` 设为 source 侧 inode，而非 target 侧。代码注释已自述："Actually we need the target's MountFSInode, not source"。

**方案**: 修正 backref 创建逻辑，确保指向 target 侧的正确 MountFSInode。

**验收**: 同 Phase 1

## Phase 3: `children` Arc → Weak

**问题**: `Ext4OSInode.children: BTreeMap<String, Arc<dyn IndexNode>>` 强引用，无界增长。

**方案**:
1. 类型改为 `BTreeMap<String, Weak<dyn IndexNode>>`
2. 读路径（`find()` line 891）：`upgrade()` 成功返回，失败删 stale entry 回退磁盘
3. 写路径（5 处 insert）：`Arc::downgrade()`
4. 加 stale prune：超过阈值时清理过期 Weak

**验收**:
- 双架构编译通过
- QEMU 跑 mount bind 测试，heap_free 不再单调下降
- children hit/miss/stale 计数器正确

## 审查流程

```
Phase N 完成 → Oracle 审查 → 人工审查 → Phase N+1 开始
```
