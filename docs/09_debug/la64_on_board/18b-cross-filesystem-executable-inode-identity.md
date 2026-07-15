---
title: "跨文件系统 inode 身份碰撞与假 ftruncate 故障复盘"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, vfs, execve, etxtbsy, inode, mountfs, ftruncate, ext4]
code_paths:
  - "os/src/fs/vfs/file_system.rs"
  - "os/src/fs/vfs/mount.rs"
  - "os/src/task/process.rs"
  - "os/src/syscall/fs.rs"
  - "os/src/syscall/process/exec.rs"
  - "os/src/fs/ramfs/mod.rs"
  - "os/src/fs/ext4/ext4fs.rs"
  - "user/src/bin/fs_test.rs"
related_docs:
  - "docs/09_debug/la64_on_board/bug-hole-read-mismatch.md"
  - "docs/09_debug/la64_on_board/18a-ext4-metadata-cache-and-inode-snapshot.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/05_process/process-control-block.md"
  - "docs/Work_Log.md"
entry_points:
  - "FileSystem::identity_key"
  - "MountFS::identity_key"
  - "inode_busy_key"
  - "is_executable_inode_busy"
  - "check_exec_metadata"
  - "test_ftruncate"
---

# 跨文件系统 inode 身份碰撞与假 ftruncate 故障复盘

## 1. 一句话结论

LA64 ext4 chroot 回归最初把第 21 项报告成 `ftruncate returned -9`。这不是 truncate 数据
路径先坏：旧测试没有检查第二次 writable `open()` 的返回值，实际 `open()` 已返回
`-ETXTBSY (-26)`；把负 errno 强转成 fd 再调用 `ftruncate`，才二次得到 `-EBADF (-9)`。

继续沿 `ETXTBSY` 唯一判定路径追踪，根因是全局 executable/writable inode busy 表使用
`(Metadata.dev_id, inode_id)` 作键，而 ramfs、tmpfs、ext4 等实现的 `dev_id` 仍是占位 0。
不同文件系统恰好分配相同 inode 号时，普通 ext4 文件会与另一个文件系统中的正在执行
文件碰撞，被误判为 text busy。

`b6c5c973` 为内核内部 busy 表改用 `(fs.identity_key(), inode_id)`，并要求 `MountFS`
转发底层 identity。修复后 LA64/RV64 的 ext4 fixture 都通过 ftruncate 和完整 `63/63`。
本问题与 ext4 truncate 实现、LA64 用户栈 ABI 问题是相互独立的故障链。

## 2. 问题卡

| 属性 | 结论 |
|------|------|
| 首个表象 | ext4 chroot `fs_test`: `FAIL: ftruncate returned -9` |
| 第一层真实错误 | writable reopen 返回 `-26`，旧测试把它遮蔽成后续 `-9` |
| 根因 | 不同 FS 共用 `dev_id=0`，相同 inode number 在全局 busy 表碰撞 |
| 影响面 | open-for-write 与 exec 双向 `ETXTBSY` 检查；所有使用占位 dev_id 的 FS 组合 |
| 严重性 | High：无关文件不能写打开，且错误随 inode 分配顺序出现 |
| 修复提交 | `b6c5c973aec727539df32592841e5bb06aefa45d` |
| 修复后证据 | LA64/RV64 全新 ext4 fixture ftruncate PASS、总计 `63/63` |
| 边界 | 无保存的专用 bind-mount 正/反向 `ETXTBSY` 矩阵；未修用户态稳定 `st_dev` |

## 3. `ETXTBSY` 在这里保护什么

MangoCore 为 Linux 兼容维护两个全局引用表：

```text
EXEC_INODE_REFS  : 当前作为进程可执行映像的 inode
WRITE_INODE_REFS : 当前被 writable open 的 inode
```

检查方向是对称的：

```text
open(O_WRONLY/O_RDWR/O_TRUNC)
  -> is_executable_inode_busy(target)
  -> busy 则 ETXTBSY

execve(file)
  -> is_writable_inode_busy(file)
  -> busy 则 ETXTBSY
```

语义本身正确：正在执行的同一个文件不应被写打开，正在写打开的同一个文件不应突然成为
可执行映像。错误发生在“同一个文件”的 key 定义不完整。

## 4. inode number 不是全局唯一 ID

inode 号只在一个文件系统实例内部唯一。下面两个对象可以合法同时存在：

```text
ramfs instance A : inode N, executable
ext4  instance B : inode N, ordinary data file
```

旧 key 是：

```text
(metadata.dev_id, metadata.inode_id)
```

理论上稳定且不同的 `st_dev` 可以区分 A/B；但当时实现现实是：

```text
ramfs metadata.dev_id = 0
tmpfs metadata.dev_id = 0
ext4  metadata.dev_id = 0
```

于是两者都变成 `(0, N)`。全局表无法再判断它们来自哪个文件系统。

这不是 hash collision，也不是锁竞态；key 在构造时已经丢失了必要维度。增加重试或清空
busy 表只会破坏正确的 `ETXTBSY` 语义。

## 5. 为什么最初看到的是 `-EBADF` 而不是 `-ETXTBSY`

### 5.1 旧测试的错误传播

旧 `test_ftruncate()` 大致执行：

```text
open(O_CREAT|O_RDWR) -> write -> close
open(O_RDWR)         -> 未检查返回值
ftruncate(fd, 6)
```

第二次 `open` 实际返回 `-26`。用户测试把它作为 `usize` fd 传给 syscall，fd table 查找
当然失败，于是 `ftruncate` 返回 `-9`：

```text
真实首错: open -> -ETXTBSY (-26)
遮蔽错误: ftruncate((usize)-26, 6) -> -EBADF (-9)
```

因此日志 `logs/ext4-fs-test-la64-chroot-20260715.log` 的：

```text
[21/63] ftruncate
  FAIL: ftruncate returned -9
```

只证明测试最终拿到了坏 fd，不证明 truncate 实现返回了错误语义。

### 5.2 增强测试把首错前移

`b6c5c973` 同时让测试检查 mkdir、首次 open、write 和 reopen。诊断阶段日志
`logs/ext4-ftruncate-diag-la64-20260715.log` 由此直接显示：

```text
[21/63] ftruncate
  FAIL: ftruncate reopen returned -26
```

问题边界随即从 `sys_ftruncate -> inode.resize` 前移到 `open_requests_write ->
is_executable_inode_busy`。

## 6. 调试追溯

### 6.1 确保测试真的命中 ext4

诊断镜像先把 `/fs_test` 复制到 `/sdcard/fs_test`，再执行：

```text
/bin/busybox chroot /sdcard /fs_test
```

日志同时显示 `/dev/vda` 识别为 raw ext4 并挂载在 `/sdcard`。因此测试中的 `/tmp14`
位于 chroot 后的 ext4，而不是初始 ramfs `/tmp`。这排除了“测错文件系统”的常见假阳性。

### 6.2 将 `-9` 拆成 setup/reopen/truncate 三段

增强断言后，首个失败明确落在 reopen，errno 从 `EBADF` 还原为 `ETXTBSY`。这一步比在
truncate 深处插桩更重要：真正失败的 syscall 已经改变。

### 6.3 沿 `ETXTBSY` 分支反查全局状态

writable open 路径只有目标被 `is_executable_inode_busy()` 命中时返回该错误。提交前
`inode_busy_key()` 和 `exec_key_from_file()` 都从 metadata 取 `(dev_id, inode_id)`。

继续检查具体 FS metadata，发现多个实现都报告占位 `dev_id=0`。Work_Log 记录本次碰撞
发生在 ramfs/ext4 的同号 inode；诊断日志未保存实际 inode 数字，因此本文不虚构
`N` 的具体值。

### 6.4 修复后回到原始行为验证

修复后两架构日志均显示：

```text
[21/63] ftruncate
  PASS: ftruncate to 6 bytes OK
  PASS: ftruncate hole zero-filled (16 bytes at offset 20)
...
=== FS Test: 63/63 passed ===
```

这证明 writable reopen、truncate 缩短、再扩展 hole 补零这条原始用户路径已恢复。

## 7. 修复设计

### 7.1 `FileSystem::identity_key()`

`FileSystem` 新增启动期实例身份：

```rust
fn identity_key(&self) -> usize {
    self as *const Self as *const () as usize
}
```

busy key 改为：

```text
(inode.fs().identity_key(), inode_id)
```

只要具体 FS 实例仍存活，不同 ramfs/ext4 对象地址不同；同一 FS 内相同 inode number
仍映射为同一 key。

### 7.2 为什么 `MountFS` 必须转发

若 wrapper 使用自身地址，底层同一文件从普通挂载点和 bind/mount wrapper 访问时会得到
不同 identity：

```text
MountFS wrapper X -> identity X
MountFS wrapper Y -> identity Y
same backing inode N
```

这样反而能绕过真正应该生效的 `ETXTBSY`。所以 `MountFS::identity_key()` 返回
`inner_filesystem.identity_key()`，让身份跟随底层文件系统实例，而不是路径视图。

### 7.3 为什么不直接给所有 FS 随便分配 `dev_id`

用户可见 `st_dev` 需要稳定语义、挂载命名空间和设备编号策略；本轮只需修内核内部全局
busy 表。用启动期实例 key 可以最小化影响，同时不谎称 `stat(2).st_dev` 已完整实现。

这也是该 key 的明确边界：它不能直接暴露给用户态，重启后也不稳定。

## 8. 代码证据

提交前：

```rust
fn inode_busy_key(inode: &Arc<dyn IndexNode>) -> Option<InodeBusyKey> {
    inode.metadata().ok().map(|meta| (meta.dev_id, meta.inode_id))
}
```

提交后：

```rust
fn inode_busy_key(inode: &Arc<dyn IndexNode>) -> Option<InodeBusyKey> {
    let inode_id = inode.metadata().ok()?.inode_id;
    Some((inode.fs().identity_key(), inode_id))
}
```

`exec_key_from_file()` 也委托同一 helper，保证 exec 注册和 writable 查询不会使用两套 key。

修复涉及：

- `os/src/fs/vfs/file_system.rs`：定义 identity contract；
- `os/src/fs/vfs/mount.rs`：转发 backing FS identity；
- `os/src/task/process.rs`：busy 表改用 FS instance identity；
- `user/src/bin/fs_test.rs`：把 setup/reopen/write 的首错直接打印出来。

## 9. 替代假设及排除

| 假设 | 证据 | 结论 |
|------|------|------|
| `ftruncate` 的 resize/hole 逻辑坏了 | errno 在调用 ftruncate 前的 reopen 已是 -26 | 排除为首错；修复后 truncate/hole 均 PASS |
| fd table 随机丢 fd | `-9` 可由未检查的 `open=-26` 确定推导 | 排除随机 fd 丢失 |
| ext4 文件确实正在执行 | 目标是新建普通 `truncfile`；碰撞来自另一 FS 同号 inode | 排除同文件 text-busy |
| inode cache 把两个 ext4 inode 混成一个 | 全局 busy key 在进入 ext4 inode cache 前已碰撞 | 排除为该错误的必要根因 |
| chroot 没生效，测试仍在 ramfs | 日志显示复制至 `/sdcard` 并 `chroot /sdcard /fs_test` | 排除测错根目录 |
| LA64 ABI 栈错位导致错误码误读 | 增强日志稳定显示 syscall reopen=-26；同修复在 rv64 也回归 | 与 ABI 问题独立 |
| 清空 EXEC_INODE_REFS 即可 | 会允许真正执行中的文件被写开 | 拒绝 workaround |

## 10. 修复为何有效

新 key 满足 busy 表真正需要的等价关系：

```text
same backing filesystem instance AND same inode number
    => same busy key

different filesystem instance, even if inode number is equal
    => different busy key
```

`MountFS` 转发又保证：

```text
same backing filesystem/inode through another mount view
    => still same busy key
```

因此既消除假阳性，也不引入 bind mount 绕过的假阴性。

## 11. 验证矩阵

| 层级 | 证据 | 结果 | 能证明什么 |
|------|------|------|------------|
| 原始表象 | `logs/ext4-fs-test-la64-chroot-20260715.log` | ftruncate `-9` | 旧测试只看到下游坏 fd |
| 首错诊断 | `logs/ext4-ftruncate-diag-la64-20260715.log` | reopen `-26` | 首错是 writable open 的 ETXTBSY |
| 代码 diff | `b6c5c973^..b6c5c973` | key 从 dev_id 改为 FS identity | 根因与修复可逐行复核 |
| LA64 行为 | `logs/ext4-fs-test-la64-fixed-20260715.log` | ftruncate 两项 PASS，`63/63` | 原触发链修复 |
| RV64 行为 | `logs/ext4-fs-test-rv64-fixed-20260715.log` | ftruncate 两项 PASS，`63/63` | 共享 VFS 修复无架构回归 |
| 磁盘结果 | `logs/fsck-ext4-fs-test-{la64,rv64}-fixed-20260715.log` | clean | 测试后 fixture 一致；不是 identity 专项证明 |

## 12. 已知边界

- 本轮没有保存一个专门控制 inode number 的最小复现日志，例如“保持 ramfs inode N
  executable busy，同时在 ext4 构造 inode N”。根因主要由 `-26` 路径、旧 key 和多个
  `dev_id=0` 的代码证据闭合。
- 没有保存 bind mount 下两条访问路径的正向/反向 `ETXTBSY` 专项日志；`MountFS` 转发由
  代码 contract 证明，仍值得补自动化测试。
- `identity_key()` 只在本次启动、该 FS 对象生命周期内有效；不能作为持久 ID、磁盘 UUID
  或用户态 `st_dev`。
- 其他仍使用 `(dev_id, inode_id)` 的全局设施（例如部分 flock/POSIX lock key）不因本次
  busy 表修复自动获得跨 FS 隔离；应另行审计，不能扩大修复结论。
- 修复消除的是 key 假阳性，不改变真正同 inode 的 Linux `ETXTBSY` 行为。
- 本问题不解释 LA64 hole-read 的用户栈 ABI 违规，也不解释 ext4 allocator/cache 的磁盘
  损坏；它们只是同一轮回归中被依次暴露。

## 13. 闭合证据链

```text
ext4 chroot fs_test 报 ftruncate -9
  -> 检查测试发现 reopen 返回值未验证
  -> 增强诊断后首错变为 reopen -26 (ETXTBSY)
  -> writable open 的 -26 只来自 executable busy 查询
  -> 旧 busy key = (dev_id, ino)
  -> ramfs/ext4 等 dev_id 都是占位 0
  -> 不同 FS 同号 inode 被压成相同 key
  -> b6c5c973 改为 (fs.identity_key(), ino)
  -> MountFS 转发底层 identity，保留同一底层 inode 的保护
  -> LA64/RV64 ftruncate 缩短+hole 两项 PASS，整套 63/63
```

这条链同时展示一个通用调试原则：**当测试打印某 syscall 的 errno 时，先确认它使用的 fd
是否由前一 syscall 成功产生；否则错误码可能只是第二现场。**

## 14. 复核命令

```bash
git diff b6c5c973^ b6c5c973 -- \
  os/src/fs/vfs/file_system.rs \
  os/src/fs/vfs/mount.rs \
  os/src/task/process.rs \
  user/src/bin/fs_test.rs

rg -n "dev_id: 0|identity_key|inode_busy_key|ETXTBSY" \
  os/src/fs os/src/task/process.rs os/src/syscall

rg -n "ftruncate.*returned|ftruncate to 6|63/63" \
  logs/ext4-fs-test-la64-chroot-20260715.log \
  logs/ext4-ftruncate-diag-la64-20260715.log \
  logs/ext4-fs-test-{la64,rv64}-fixed-20260715.log
```
