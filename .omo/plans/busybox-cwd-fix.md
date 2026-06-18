# BusyBox cwd / relative path / shell redirection 修复计划

## 当前架构发现

### FsStatus — 只有 `working_inode`，无 `working_path`

```rust
// os/src/task/task.rs:30-33
pub struct FsStatus {
    pub working_inode: Arc<vfs::File>,
}
```

### getcwd 链路 — 完全依赖 `absolute_path()`

```
sys_getcwd (syscall/fs.rs:249)
  → File::get_cwd() (vfs/file.rs:978)
    → self.inode.absolute_path() (vfs/mount.rs:402)
      → parent.inner_inode.get_entry_name(ino_id) (mount.rs:419)
        → unwrap_or_else(|_| "?")  ← Ext4OSInode 未实现 → 永远返回 "?"
```

### sys_chdir — 正确更新 inode，但无 path 维护

```
sys_chdir (syscall/fs.rs:1258-1260)
  → lock.working_inode.cd(&path)
    → vfs_lookup + File::new
  → lock.working_inode = new_working_inode  // inode 正确，path 无维护
```

### AT_FDCWD — 使用正确 inode

```rust
// syscall/fs.rs:75
AT_FDCWD => task.fs.lock().working_inode.inode.clone()
```

### execve — cwd 通过 task.fs Arc 继承

```rust
// syscall/process.rs:801
let working_inode = &task.fs.lock().working_inode;
// ... load_elf on SAME task → cwd 保留
```

## 问题诊断

### P1: "busybox pwd → /?"
**根因**: `absolute_path()` 依赖 `get_entry_name()` — Ext4OSInode 未实现 → 返回 "?"
**影响**: pwd 显示错误，但不影响文件操作（inode 正确）
**修复方向**: FsStatus 加 `working_path: String`，getcwd 优先返回此字段

### P2: "touch test.txt 后 cat test.txt → ENOENT"
**假设 1**: 每个 busybox 命令是独立 exec，cwd inode 继承正确 → 不应对
**假设 2**: shell redirection 的 open/dup2/close 序列有问题
**假设 3**: O_CREAT 创建的 direntry 在后续 lookup 中不可见（children cache 掩盖）
**需要**: syscall trace 确认

### P3: 跨 separate exec 的 cwd 可见性
**假设**: initproc 或 shell 的 cwd 状态可能在被调用命令中丢失
**需要**: trace execve 前后 cwd inode

---

## Phase 0: 建立 Oracle 对照器

### 目标
Linux host BusyBox 输出 vs MangoCore BusyBox 输出对照表

### 操作
1. 在 Docker 容器内安装 busybox-static（如果可用）
2. 对每个测试 case 记录 stdout/stderr/exit code
3. 对 MangoCore 侧：启用 syscall trace (LOG=info)，收集输出
4. 输出差异表

### 测试 case 清单
```
A1: busybox pwd
A2: busybox sh -c 'pwd'
A3: busybox sh -c 'cd /; pwd'
A4: busybox sh -c 'cd /tmp; pwd'
A5: busybox sh -c 'cd /tmp; cd ..; pwd'

B1: busybox sh -c 'touch /tmp/bb.txt; ls -l /tmp/bb.txt; stat /tmp/bb.txt; rm /tmp/bb.txt'
B2: busybox sh -c 'echo hello > /tmp/bb.txt; cat /tmp/bb.txt; stat /tmp/bb.txt; rm /tmp/bb.txt'

C1: busybox sh -c 'pwd; touch test.txt; ls -l test.txt; stat test.txt; rm test.txt'
C2: busybox sh -c 'pwd; echo hello > test.txt; cat test.txt; stat test.txt; rm test.txt'
C3: busybox sh -c 'echo aaa > test.txt; echo bbb >> test.txt; cat test.txt; rm test.txt'

D1: busybox touch test.txt; busybox ls -l test.txt; busybox cat test.txt; busybox stat test.txt; busybox rm test.txt

E1: busybox sh -c 'cp busybox_cmd.txt busybox_cmd.bak; ls -l busybox_cmd.bak; stat busybox_cmd.bak; rm busybox_cmd.bak'
```

### 验收
- 形成 oracle 差异表
- 明确 FAIL/PASS 分布
- 归类问题到 getcwd / relative path / redirection / 其他

---

## Phase 1: syscall trace 定向定位

### 目标
对 Phase 0 中的所有 FAIL case 打定向 trace

### 需要 trace 的 syscall
openat, close, dup2, dup3, write, read, execve, chdir, getcwd, unlinkat, stat/fstatat, getdents64

### trace 格式
每个 syscall 打印: pid, syscall名, 关键参数, 返回值

### 关键 trace 点
- `resolve_start_inode`: dirfd 值, 返回的 inode 信息
- `sys_execve`: exec 前后 cwd inode id
- `sys_chdir`: old/new cwd inode id + resolved path
- `sys_getcwd`: absolute_path() 原始返回值
- `sys_openat`: resolved start inode, leaf name, flags, 返回值

### 验收
必须能回答：
1. `touch test.txt` 在哪个目录创建？
2. `cat test.txt` 在哪个目录查找？
3. `echo > test.txt` 的 redirection open 是否成功？
4. dup2 是否正确？
5. execve 后 cwd 是否一致？
6. pwd 为什么输出 "/?"？

---

## Phase 2: 修复 getcwd / cwd 表示

### 目标
cwd 不再依赖 broken 的 `absolute_path()`

### 设计方案
在 `FsStatus` 添加 `working_path: String`:
```rust
pub struct FsStatus {
    pub working_inode: Arc<vfs::File>,
    pub working_path: String,  // 新增
}
```

### 实现细节
1. **初始化**: initproc 创建时 `working_path = "/"`
2. **fork**: 继承 working_path
3. **chdir**: 
   - 绝对路径: `working_path = normalized(path)`
   - 相对路径: `working_path = normalized(working_path + "/" + path)`
4. **getcwd**: 优先返回 `working_path`，失败再 fallback `absolute_path()`
5. **path normalization**: 处理 ".", "..", "//", trailing "/"
6. **execve**: 不变（FsStatus 在 Arc 内，自动保留）

### 不做的
- 不实现 `get_entry_name()` for ext4（保留 broken 作为 fallback）
- 不实现 full dentry chain
- 不处理 cwd 被 unlink/rename 的复杂语义（标记为 known limitation）

### 验收
1. `busybox pwd` → "/"
2. `busybox sh -c 'cd /tmp; pwd'` → "/tmp"
3. FS test 51/51 保持
4. read/readlink/read_via_symlink 0 I/O 保持

---

## Phase 3: 修复 AT_FDCWD / relative path / shell redirection

### 目标
基于 Phase 1 trace 结果，修复相对路径文件可见性问题

### 可能修复点（按 trace 结果确定）
1. **O_CREAT 后 direntry 不可见** → children cache 问题？
2. **dup2 后文件内容不可见** → fd table 操作问题？
3. **close-on-exec 错误清除** → FdTable flag 处理？
4. **shell redirection 未实现** → open+dup2+close 序列缺失？

### 验收
1. `touch test.txt` 后 `cat test.txt` 能找到文件
2. `echo > test.txt` 后文件内容可读
3. `cp + rm` 成功

---

## Phase 4: create-heavy metadata 打盘审计

### 目标
审计 create/write path 的 metadata 写放大

### 新增 counter
- flush reason counters（8 个）
- sb/gd write reason counters（10 个）

### 输出报告
回答:
1. 每个 create 多少次 inode table write？
2. 64KB write 为什么 104 inode flush？
3. 哪些可做 operation-local coalescing？

### 禁止
- 不上 DirtyBlockDevice
- 不跨 syscall cache metadata

---

## Phase 5: cache memory lifecycle

### 目标
验证 cache 不会只进不出

### 验证方式
`sys_ext4_counters cmd 6/8/9` 的 dump + prune 接口（已在上一版实现）

---

## Final Acceptance

1. FS test 51/51
2. busybox pwd 不再 "/?"
3. 相对路径文件操作正确
4. read/readlink/read_via_symlink 0 I/O
5. single fast symlink 不回退
