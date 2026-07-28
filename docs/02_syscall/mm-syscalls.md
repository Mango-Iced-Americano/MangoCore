---
title: "内存管理 syscall"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [syscall, mm, mmap, brk]
---

# 内存管理 syscall

## 1. 概述

内存管理 syscall 的参数解析位于 `os/src/syscall/process/mm.rs`。该文件把用户可见 ABI 转换为 `mm` 子系统的地址空间操作：

```
syscall/mod.rs
  ├── SYSCALL_BRK     -> sys_brk()
  ├── SYSCALL_MMAP    -> sys_mmap()
  ├── SYSCALL_MUNMAP  -> sys_munmap()
  ├── SYSCALL_MREMAP  -> sys_mremap()
  ├── SYSCALL_MPROTECT -> sys_mprotect()
  ├── SYSCALL_MLOCK*  -> sys_mlock*()
  ├── SYSCALL_MINCORE -> sys_mincore()
  ├── SYSCALL_MADVISE -> sys_madvise()
  └── process_vm_* / pkey / membarrier / riscv_flush_icache
```

核心 MM 语义由 `os/src/mm/mmap.rs`、`address_space.rs`、`vma_set.rs`、`page_fault.rs`、`filemap.rs` 执行。syscall 层负责 Linux ABI 参数、错误优先级、用户指针和 fd/file 预检查。

## 2. prot 与 flags 解析

### 2.1 `parse_mmap_prot`

允许的 prot 位：

| 常量 | 值 | 映射 |
|------|----|------|
| `PROT_READ` | `0x1` | `MapPermission::R` |
| `PROT_WRITE` | `0x2` | `MapPermission::R | MapPermission::W` |
| `PROT_EXEC` | `0x4` | `MapPermission::X` |

写权限会同时添加读权限：

```rust
if prot & PROT_WRITE != 0 {
    map_perm |= MapPermission::R | MapPermission::W;
}
```

原因写在源码注释中：部分架构上只有写无读会导致反复页故障。

未知 prot 位返回 `EINVAL`。

### 2.2 `parse_mmap_flags`

映射类型必须是：

| 类型 | 说明 |
|------|------|
| `MAP_SHARED` | shared 映射 |
| `MAP_PRIVATE` | private 映射 |
| `MAP_SHARED_VALIDATE` | shared validate 映射 |

类型位不是三者之一时返回 `EINVAL`。当类型是 `MAP_SHARED_VALIDATE` 且 flags 含未知位时返回 `EOPNOTSUPP`。其他类型的未知位通过 `MapFlags::from_bits_truncate(flags)` 截断。

## 3. brk / sbrk

### 3.1 `sys_sbrk`

`sys_sbrk(increment)`：

```
task = current_task()
vm = task.process.vm()
new_addr = vm.lock().sbrk(increment)
return new_addr
```

它把增量直接交给地址空间的 `sbrk()`，返回新 program break。

### 3.2 `sys_brk`

`sys_brk(brk_addr)`：

| 输入 | 行为 |
|------|------|
| `brk_addr == 0` | 查询当前 break，等价 `sbrk(0)` |
| `brk_addr < former_addr` | 计算负增量；若差值超过 `isize::MAX`，warn 后使用 0 增量 |
| `brk_addr > former_addr` | 计算正增量；若差值超过 `isize::MAX`，warn 后使用 0 增量 |

真正的 heap 上下界和匿名映射/munmap 操作由 `AddressSpace::sbrk()` 和 `mm/mmap.rs::do_sbrk()` 实现。

`sys_brk()` 只把目标地址转换成 `sbrk()` 增量：

```rust
pub fn sys_brk(brk_addr: usize) -> isize {
    let task = current_task().unwrap();
    let vm = task.process.vm();
    let mut memory_set = vm.lock();
    let new_addr = if brk_addr == 0 {
        memory_set.sbrk(0)
    } else {
        let former_addr = memory_set.sbrk(0);
        let grow_size = if brk_addr < former_addr {
            let delta = former_addr - brk_addr;
            if delta > isize::MAX as usize {
                warn!(
                    "[sys_brk] shrink delta too large: brk_addr={:X}, former_addr={:X}",
                    brk_addr, former_addr
                );
                0
            } else {
                -(delta as isize)
            }
        } else {
            let delta = brk_addr - former_addr;
            if delta > isize::MAX as usize {
                warn!(
                    "[sys_brk] grow delta too large: brk_addr={:X}, former_addr={:X}",
                    brk_addr, former_addr
                );
                0
            } else {
                delta as isize
            }
        };
        memory_set.sbrk(grow_size)
    };

    new_addr as isize
}
```

## 4. mmap

### 4.1 fd 优先级

`sys_mmap()` 首先处理 fd：

```rust
let fd_file = if flags & MAP_ANONYMOUS == 0 {
    match fd_table.get_file(fd) {
        Ok(file) => Some(file),
        Err(_) => return EBADF,
    }
} else {
    None
};
```

因此非匿名映射的坏 fd 优先返回 `EBADF`，早于 `len == 0`、prot、flags 等后续检查。

`sys_mmap()` 的主体先处理 fd 和参数，再进入 `AddressSpace::mmap()`：

```rust
pub fn sys_mmap(
    start: usize,
    len: usize,
    prot: usize,
    flags: usize,
    fd: usize,
    offset: usize,
) -> isize {
    let (files_ref, vm_ref) = {
        let task = current_task().unwrap();
        (task.process.files(), task.process.vm())
    };
    let fd_file = if flags & MapFlags::MAP_ANONYMOUS.bits() == 0 {
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(file) => Some(file),
            Err(_) => return EBADF,
        }
    } else {
        None
    };
    if len == 0 {
        return EINVAL;
    }
    let prot = match parse_mmap_prot(prot) {
        Ok(prot) => prot,
        Err(errno) => return errno,
    };
    let mut flags = match parse_mmap_flags(flags) {
        Ok(flags) => flags,
        Err(errno) => return errno,
    };
    let mut may_write = true;
    let mut write_sealed = false;
    let map_file = if flags.contains(MapFlags::MAP_ANONYMOUS) {
        None
    } else {
        if offset & (PAGE_SIZE - 1) != 0 || offset > isize::MAX as usize {
            return EINVAL;
        }
        let file = fd_file.as_ref().unwrap();
        if file.readable().is_err() {
            return EACCES;
        }
        let file_writable = file.writable().is_ok();
        if flags.contains(MapFlags::MAP_SHARED)
            && prot.contains(MapPermission::W)
            && !file_writable
        {
            return EACCES;
        }
        let seals = file.memfd_seal_bits().unwrap_or(0);
        let sealed_against_write = (seals
            & (vfs::F_SEAL_WRITE | vfs::F_SEAL_FUTURE_WRITE))
            != 0;
        if flags.contains(MapFlags::MAP_SHARED) && sealed_against_write {
            write_sealed = true;
            if prot.contains(MapPermission::W) {
                return EPERM;
            }
        }
        if flags.contains(MapFlags::MAP_SHARED) {
            may_write = file_writable;
        }
        let inode = vfs::MountFSInode::unwrap_inode(&file.inode);
        let is_zero = inode.as_any_ref().is::<crate::fs::dev::zero::Zero>();
        if !is_zero && !matches!(file.file_type(), vfs::FileType::File) {
            return EACCES;
        }
        if is_zero {
            flags |= MapFlags::MAP_ANONYMOUS;
            None
        } else {
            Some(inode)
        }
    };

    let mut memory_set = vm_ref.lock();
    memory_set.mmap(
        start,
        len,
        prot,
        flags,
        offset,
        map_file,
        may_write,
        write_sealed,
    )
}
```

### 4.2 参数检查顺序

```
非匿名 fd 查找
len == 0 -> EINVAL
parse_mmap_prot()
parse_mmap_flags()
匿名/文件分支
  offset 页对齐和 isize 上限
  file.readable()
  shared writable 需要 file.writable()
  memfd seal
  /dev/zero 转匿名
  非 regular 且非 /dev/zero -> EACCES
memory_set.mmap(...)
```

### 4.3 文件映射检查

| 场景 | errno/行为 |
|------|------------|
| offset 非页对齐 | `EINVAL` |
| offset 超过 `isize::MAX` | `EINVAL` |
| 文件不可读 | `EACCES` |
| `MAP_SHARED` + `PROT_WRITE` 但文件不可写 | `EACCES` |
| memfd `F_SEAL_WRITE` 或 `F_SEAL_FUTURE_WRITE` + shared | 标记 `write_sealed`; 若 prot 包含 W 返回 `EPERM` |
| shared 文件可写性 | `may_write = file_writable` |
| `/dev/zero` | 强制添加 `MAP_ANONYMOUS`，不传 file inode |
| 非 regular 文件且非 `/dev/zero` | `EACCES` |

最终调用：

```rust
memory_set.mmap(start, len, prot, flags, offset, map_file, may_write, write_sealed)
```

`MAP_FIXED`、`MAP_FIXED_NOREPLACE`、VMA hole 搜索、匿名 shared eager frame 等行为在 `mm/mmap.rs` 和 `AddressSpace` 中实现。

## 5. munmap / mremap

### 5.1 `sys_munmap`

`sys_munmap(start, len)` 直接调用当前进程 VM：

```rust
task.process.vm().lock().munmap(start, len)
```

成功返回 0，失败返回 MM 层 errno。

### 5.2 `sys_mremap`

支持 flags：

| flag | 值 | 约束 |
|------|----|------|
| `MREMAP_MAYMOVE` | `0x1` | 允许移动 |
| `MREMAP_FIXED` | `0x2` | 必须同时有 `MAYMOVE` |
| `MREMAP_DONTUNMAP` | `0x4` | 必须有 `MAYMOVE` 且 old_len == new_len |

检查顺序：

| 检查 | 失败 |
|------|------|
| old_addr 非页对齐 | `EINVAL` |
| flags 含未知位 | `EINVAL` |
| old_size/new_size round up 为 0 或溢出 | `EINVAL` |
| old range 超出用户空间 | `EINVAL` |
| fixed 但没有 maymove | `EINVAL` |
| dontunmap 约束不满足 | `EINVAL` |
| fixed new_addr 非页对齐/越界/与旧范围重叠 | `EINVAL` |

路径：

| 场景 | 行为 |
|------|------|
| 不移动且 new_len <= old_len | 收缩尾部 `munmap` 后返回 old_addr |
| 不允许移动但需要扩展 | 在尾部 `MAP_FIXED_NOREPLACE` 匿名映射；失败返回 `ENOMEM` |
| 允许移动 | mmap 新区域，复制 `min(old_size,new_size,old_len,new_len)`，按需 munmap 旧区 |

复制用户范围使用 `copy_current_user_range()`，每次最多一页，并要求源/目标翻译结果都是单片页内 buffer。

## 6. mprotect 与 pkey

### 6.1 `sys_mprotect`

`sys_mprotect(addr, len, prot)` 先复用 `parse_mmap_prot()`，再调用：

```rust
task.process.vm().lock().mprotect(addr, len, prot)
```

权限、shared writable、seal、VMA 拆分和 TLB 刷新由 MM 层处理。

### 6.2 pkey 兼容入口

| syscall | 行为 |
|---------|------|
| `pkey_mprotect(addr,len,prot,0)` | 等价 `mprotect` |
| `pkey_mprotect(..., PKEY_NO_ACCESS_RIGHTS_KEY)` | 使用原 prot |
| `pkey_mprotect(..., PKEY_ACCESS_KEY)` | effective prot = 0 |
| `pkey_mprotect(..., PKEY_WRITE_KEY)` | 清除 `PROT_WRITE` |
| 其他 pkey | `EINVAL` |

`pkey_alloc(flags, access_rights)` 要求 flags 为 0，按 access_rights 返回固定 key：no-access=1、access=2、write=3；包含 `PKEY_DISABLE_EXECUTE` 或未知组合返回 `EINVAL`。`pkey_free()` 只接受 1/2/3。

`pkey_mprotect` 会把 pkey 转成有效 prot 后复用 `sys_mprotect()`：

```rust
pub fn sys_pkey_mprotect(addr: usize, len: usize, prot: usize, pkey: isize) -> isize {
    let effective_prot = match pkey {
        0 => prot,
        PKEY_NO_ACCESS_RIGHTS_KEY => prot,
        PKEY_ACCESS_KEY => 0,
        PKEY_WRITE_KEY => prot & !PROT_WRITE,
        _ => return EINVAL,
    };

    sys_mprotect(addr, len, effective_prot)
}
```

## 7. 锁页

### 7.1 权限判断

`CAP_IPC_LOCK = 14`。`mlock`/`mlock2`/`mlockall` 判断：

```rust
inner.euid == 0 || (inner.cap_effective & (1u64 << CAP_IPC_LOCK)) != 0
```

非特权任务受 `memlock_limit_cur` 限制。

### 7.2 syscall 行为

| syscall | 行为 |
|---------|------|
| `mlock` | 调用 `vm.mlock(addr, len)`；非特权时 locked_len 超过 rlimit 返回 `ENOMEM`，limit 为 0 返回 `EPERM` |
| `mlock2` | flags 只能为 0 或 `MLOCK_ONFAULT`；0 时退化为 `mlock`；ONFAULT 调 `mlock_onfault` |
| `munlock` | 调用 `vm.munlock(addr, len)` |
| `mlockall` | flags 必须包含 `MCL_CURRENT`/`MCL_FUTURE`/`MCL_ONFAULT` 且非 0；`MCL_CURRENT` 分支检查 mapped bytes 和 memlock limit |
| `munlockall` | 调用 `vm.munlockall()` |

## 8. mincore

`sys_mincore(addr, len, vec)`：

| 检查 | 失败 |
|------|------|
| addr 非页对齐 | `EINVAL` |
| len round up 溢出 | `ENOMEM` |
| rounded_len 为 0 | 成功返回 0 |
| 范围超过 `USER_VA_END` | `ENOMEM` |
| vec 用户 buffer 不可写 | `EFAULT` |
| residency Vec reserve 失败 | `ENOMEM` |

随后调用：

```rust
task.process.vm().lock().mincore(addr, rounded_len, residency.as_mut_slice())
copy_to_user_array(token, residency.as_ptr(), vec as *mut u8, page_count)
```

## 9. madvise

`sys_madvise(addr, length, advice)` 要求 addr 页对齐，length round up 不溢出且范围在用户空间内。length 为 0 成功返回。

支持的 advice：

| 类别 | advice |
|------|--------|
| 访问模式 | `MADV_NORMAL`, `MADV_RANDOM`, `MADV_SEQUENTIAL`, `MADV_WILLNEED` |
| 释放/回收 | `MADV_DONTNEED`, `MADV_FREE`, `MADV_COLD`, `MADV_PAGEOUT` |
| fork 行为 | `MADV_DONTFORK`, `MADV_DOFORK`, `MADV_WIPEONFORK`, `MADV_KEEPONFORK` |
| 兼容位 | `MADV_MERGEABLE`, `MADV_UNMERGEABLE`, `MADV_HUGEPAGE`, `MADV_NOHUGEPAGE`, `MADV_DONTDUMP`, `MADV_DODUMP` |

未知 advice 返回 `EINVAL`。具体 VMA 标记和页释放由 `AddressSpace::madvise()`/`VmaSet` 执行。

## 10. remap_file_pages

`sys_remap_file_pages(addr, size, prot, pgoff, flags)`：

| 检查 | 行为 |
|------|------|
| `prot != 0` | `EINVAL` |
| flags 含非 `MAP_NONBLOCK` 位 | `EINVAL` |
| size round up 为 0 或溢出 | `EINVAL` |
| 范围越界 | `EINVAL` |
| 所有检查通过 | 仍返回 `EINVAL` |

该入口保留 ABI 兼容和参数校验，但不执行重映射。

## 11. membarrier 与 icache

### 11.1 `riscv_flush_icache`

`flags` 只能包含 `SYS_RISCV_FLUSH_ICACHE_LOCAL = 1`。未知 flags 返回 `EINVAL`。rv64 下执行 `fence.i`；其他架构下不执行汇编但返回成功。

### 11.2 `membarrier`

支持命令：

| cmd | 行为 |
|-----|------|
| `MEMBARRIER_CMD_QUERY` | 返回 supported bitmask |
| `MEMBARRIER_CMD_GLOBAL` | 成功 |
| `MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED` | 设置当前任务 `membarrier_private_expedited_registered = true` |
| `MEMBARRIER_CMD_PRIVATE_EXPEDITED` | 已注册返回成功，否则 `EPERM` |

flags 必须为 0，否则 `EINVAL`。

## 12. 跨进程 VM

| syscall | 分支 |
|---------|------|
| `process_vm_readv` | `sys_process_vm_readv(pid, local_iov, liovcnt, remote_iov, riovcnt, flags)` |
| `process_vm_writev` | `sys_process_vm_writev(pid, local_iov, liovcnt, remote_iov, riovcnt, flags)` |

远程进程查找、权限、iovec 和地址访问在同文件后续函数中实现。该路径不能用普通 `current_user_vm(token)` 直接访问远程地址空间。

## 13. get_mempolicy 和 memfd 关联

`get_mempolicy` 分支注册到 `sys_get_mempolicy(...)`，用于 NUMA/内存策略兼容。`memfd_create` 位于 `syscall/fs.rs`，但它影响 `mmap`：`sys_mmap()` 会读取 `file.memfd_seal_bits()`，shared file mapping 若遇到 `F_SEAL_WRITE | F_SEAL_FUTURE_WRITE` 会设置 `write_sealed`，并在带写权限时返回 `EPERM`。

## 14. 错误码边界表

| 场景 | errno |
|------|-------|
| 非匿名 mmap 坏 fd | `EBADF` |
| mmap len 为 0 | `EINVAL` |
| mmap prot 未知位 | `EINVAL` |
| `MAP_SHARED_VALIDATE` 未知 flag | `EOPNOTSUPP` |
| 文件 mmap offset 非页对齐 | `EINVAL` |
| mmap 文件不可读 | `EACCES` |
| shared writable mmap 但文件不可写 | `EACCES` |
| memfd seal 阻止 shared writable mmap | `EPERM` |
| 非 regular 文件 mmap 且不是 `/dev/zero` | `EACCES` |
| `MAP_FIXED_NOREPLACE` 覆盖已有 VMA | 由 MM 层返回 `EEXIST` |
| mremap flags/范围不合法 | `EINVAL` |
| mlock 非特权且 limit 为 0 | `EPERM` |
| mlock 非特权且超过 limit | `ENOMEM` |
| mincore vec 不可写 | `EFAULT` |
| remap_file_pages 已校验后 | `EINVAL` |

MM syscall 的 errno 优先级通常体现 Linux 语义：例如非匿名 `mmap` 的坏 fd 要先返回 `EBADF`，再谈 prot/flags 和文件权限；用户输出 buffer 如 `mincore` vec 不可写，应在写回前通过 uaccess 检出 `EFAULT`；`MAP_SHARED_VALIDATE` 的未知 flag 返回 `EOPNOTSUPP`，不同于普通未知 prot 的 `EINVAL`。

读 MM syscall 时要区分“syscall 参数层”和“地址空间层”。`syscall/process/mm.rs` 负责把 fd、prot、flags、用户指针转成内核对象和 bitflags；`mm/mmap.rs`、`vma_set.rs`、`address_space.rs` 才负责 VMA 创建、拆分、权限修改、resident 判断和缺页后的真实映射。

## 15. 测试映射

| 功能 | 测试 |
|------|------|
| brk/sbrk | libc malloc、LTP `brk*` |
| mmap/munmap | LTP `mmap*`, `munmap*`, `mmapstress*` |
| mprotect/pkey | LTP `mprotect*`, pkey 兼容用例 |
| mremap | LTP `mremap*` |
| mlock/mincore/madvise | LTP `mlock*`, `mincore*`, `madvise*` |
| process_vm | LTP `process_vm_readv*`, `process_vm_writev*` |
| memfd seal + mmap | memfd/mmap seal 相关用例 |
| icache/membarrier | riscv flush icache、membarrier 兼容测试 |

## 16. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/syscall/process/mm.rs` | MM syscall 参数解析和 errno 优先级 |
| `os/src/mm/mmap.rs` | `do_mmap()`、`do_sbrk()` |
| `os/src/mm/address_space.rs` | 地址空间操作 |
| `os/src/mm/vma_set.rs` | VMA 范围操作、mprotect/mincore/madvise |
| `os/src/mm/page_fault.rs` | 缺页动作分类 |
| `os/src/mm/filemap.rs` | 文件映射 fault |
| `os/src/syscall/fs.rs` | `memfd_create` 和文件 fd 辅助 |
