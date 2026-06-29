---
title: "clone、clone3、unshare 与 namespace"
category: process
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [process, clone, namespace, pidfd, vfork]
---

# clone、clone3、unshare 与 namespace

## 1. 源码位置

clone 系列 syscall 位于 `os/src/syscall/process/clone.rs`，实际任务/进程创建由 `os/src/task/task.rs::TaskControlBlock::sys_clone()` 完成。

| 源码 | 函数/对象 | 作用 |
|------|-----------|------|
| `os/src/syscall/process/clone.rs` | `sys_clone()` | 传统 clone ABI，按架构寄存器约定传入 flags、stack、ptid、tls、ctid。 |
| `os/src/syscall/process/clone.rs` | `sys_clone3()` | clone3 结构体 ABI，读取 `CloneArgsV0/CloneArgs` 后归一到共用路径。 |
| `os/src/syscall/process/clone.rs` | `sys_clone_inner()` | clone/clone3 共用实现，完成 flags 校验、child 创建、tid 写回、pidfd、发布和调度。 |
| `os/src/syscall/process/clone.rs` | `sys_unshare()` | 当前进程解除共享资源，覆盖 FS、UTS、NET、NS、IPC 等已接入对象。 |
| `os/src/syscall/process/clone.rs` | `sys_setns()` | 通过 `/proc/[pid]/ns/*` fd 切换 network、mount、IPC namespace。 |
| `os/src/task/task.rs` | `TaskControlBlock::sys_clone()` | 构造 child TCB/PCB/VM、trap context、内核栈、TLS 和共享/复制资源。 |
| `os/src/task/task.rs` | `publish_clone_child()` | 把 child 链接到父进程 children，并处理 `CLONE_PARENT` 语义。 |
| `os/src/task/process_manager.rs` | `ProcessManager::publish_clone_child()` / `schedule_clone_child()` | clone 发布和调度入口，`CLONE_VFORK` 父等待在这里接入。 |
| `os/src/task/process.rs` | `uts()/net()/mnt()/ipc()` 与 `unshare_*()/set_*()` | 进程级 namespace 句柄的读取、复制和替换。 |
| `os/src/task/mount_namespace.rs` | `MountNamespace` | mount namespace 标识和全局初始对象。 |
| `os/src/task/net_namespace.rs` | `NetNamespace` | network namespace 标识、设备表、路由表和按 pid 注册表。 |
| `os/src/task/ipc_namespace.rs` | `IpcNamespace` | SysV IPC namespace 标识和全局初始对象。 |
| `os/src/fs/procfs/pid/ns.rs` | `NetNsFile` / `MntNsFile` / `IpcNsFile` | `/proc/[pid]/ns/*` fd 对象，供 `setns()` downcast 取得目标 namespace。 |

## 2. CloneFlags

当前定义的 clone flags：

| 标志 | 语义 |
|------|------|
| `CLONE_VM` | 共享地址空间 |
| `CLONE_FS` | 共享 cwd/root/umask |
| `CLONE_FILES` | 共享 fd table |
| `CLONE_SIGHAND` | 共享 signal action 表 |
| `CLONE_PIDFD` | 在父进程 fd table 中创建 pidfd |
| `CLONE_VFORK` | 父线程等待 child exec 或 exit |
| `CLONE_PARENT` | child parent 使用调用者的 parent |
| `CLONE_THREAD` | child 作为同一线程组线程 |
| `CLONE_NEWNS` | 新 mount namespace |
| `CLONE_SYSVSEM` | flag 定义存在，当前不建立独立 sem undo 语义 |
| `CLONE_SETTLS` | 设置 child TLS |
| `CLONE_PARENT_SETTID` | 向父地址空间写 child tid |
| `CLONE_CHILD_CLEARTID` | child 退出时清零并 futex wake |
| `CLONE_CHILD_SETTID` | 向 child 地址空间写 child tid |
| `CLONE_NEWUTS` | 新 UTS namespace |
| `CLONE_NEWIPC` | 新 IPC namespace |
| `CLONE_NEWNET` | 新 network namespace |
| `CLONE_NEWUSER/NEWPID/NEWCGROUP/IO` | flag 定义存在，clone 路径不创建对应独立资源对象 |

低 8 位作为 `exit_signal`，不放入 `CloneFlags`。

## 3. 基础校验

`sys_clone_inner()` 在创建任务前执行：

| 条件 | 错误 |
|------|------|
| 空闲物理页少于 32 | `ENOMEM` |
| `CLONE_SIGHAND` 无 `CLONE_VM` | `EINVAL` |
| `CLONE_THREAD` 无 `CLONE_SIGHAND` | `EINVAL` |
| `CLONE_VFORK` 与 `CLONE_THREAD` 同时存在 | `EINVAL` |
| `CLONE_NEWNS` 与 `CLONE_FS` 同时存在 | `EINVAL` |
| `CLONE_NEWUTS/NEWNET/NEWNS/NEWIPC` 且 euid 非 0 | `EPERM` |
| `CLONE_PIDFD` 与 `CLONE_PARENT_SETTID` 同时用于传统 clone | `EINVAL` |

exit signal 无效时不会直接失败，而是记录 warning，并使用空 signal。

## 4. clone 主流程

```
sys_clone_inner()
  ├── 校验 flags 与权限
  ├── parent.sys_clone()
  │     ├── TaskQuotaGuard::try_acquire()
  │     ├── 复制或共享 VM
  │     ├── 选择 user_res_slot_allocator
  │     ├── tid_alloc()
  │     ├── 创建或共享 ProcessControlBlock
  │     ├── kstack_alloc()
  │     ├── 分配/查找 trap context
  │     ├── 构造 child TCB
  │     ├── 设置 child trap context
  │     ├── 复制 SysV shm attachment
  │     └── 注册 task/process
  ├── la64 分配 ASID
  ├── 写 parent_tid / child_tid
  ├── 创建 pidfd
  ├── publish_clone_child()
  ├── schedule_clone_child()
  └── 父返回 child tid
```

任何发布前失败都会调用 `cleanup_unpublished_clone()`，释放共享 VM 下已分配的用户资源。

clone 路径最重要的边界是“发布前”和“发布后”。发布前 child 还没有进入父进程 children，也没有进入 ready queue，失败可以直接回滚内核对象和用户资源槽位；发布后 child 可能被调度运行，父进程也可能 wait 到它，错误处理就不能再假装 child 不存在。因此 pidfd、parent_tid/child_tid、children 链接和调度入队的顺序必须保持一致。

`TaskQuotaGuard::try_acquire()` 位于实际构造前，用来限制线程/任务数量；`tid_alloc()` 和 `kstack_alloc()` 失败会在发布前返回。共享 VM 时分配的 user resource slot 需要特别清理，因为地址空间对象由父子共享，失败遗留的 trap context/默认栈槽位会污染父进程后续 clone。

### 4.1 clone ABI 结构和 flags

`clone3` 用户 ABI 先读入 `CloneArgsV0/CloneArgs`，传统 `clone` 直接由寄存器传参。两者最后都会归一到 `sys_clone_inner()`。

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CloneArgsV0 {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

bitflags! {
    pub struct CloneFlags: u32 {
        const CLONE_VM              =   0x00000100;
        const CLONE_FS              =   0x00000200;
        const CLONE_FILES           =   0x00000400;
        const CLONE_SIGHAND         =   0x00000800;
        const CLONE_PIDFD           =   0x00001000;
        const CLONE_PTRACE          =   0x00002000;
        const CLONE_VFORK           =   0x00004000;
        const CLONE_PARENT          =   0x00008000;
        const CLONE_THREAD          =   0x00010000;
        const CLONE_NEWNS           =   0x00020000;
        const CLONE_SYSVSEM         =   0x00040000;
        const CLONE_SETTLS          =   0x00080000;
        const CLONE_PARENT_SETTID   =   0x00100000;
        const CLONE_CHILD_CLEARTID  =   0x00200000;
        const CLONE_DETACHED        =   0x00400000;
        const CLONE_UNTRACED        =   0x00800000;
        const CLONE_CHILD_SETTID    =   0x01000000;
        const CLONE_NEWCGROUP       =   0x02000000;
        const CLONE_NEWUTS          =   0x04000000;
        const CLONE_NEWIPC          =   0x08000000;
        const CLONE_NEWUSER         =   0x10000000;
        const CLONE_NEWPID          =   0x20000000;
        const CLONE_NEWNET          =   0x40000000;
        const CLONE_IO              =   0x80000000;
    }
}
```

`CloneFlags` 只保存高位 flag；低 8 位在 `sys_clone_inner()` 中先解释成 `exit_signal`，随后通过 `flags & !0xff` 去除。因此 `exit_signal` 与 clone flag 在实现中是两个阶段。

### 4.2 `sys_clone_inner()` 的校验和发布边界

`sys_clone_inner()` 是传统 clone 与 clone3 的共同收敛点。它先做内存余量、flag 依赖和 namespace 权限校验，再构造 child；child 在 parent/child tid 和 pidfd 写入完成前不会被发布。

```rust
fn sys_clone_inner(
    flags: u32,
    stack: *const u8,
    ptid: *mut u32,
    tls: usize,
    ctid: *mut u32,
    pidfd_ptr: Option<*mut u32>,
) -> isize {
    if crate::mm::unallocated_frames() < 32 {
        warn!("[sys_clone] Low physical memory, rejecting clone");
        return -(SyscallErr::ENOMEM as isize);
    }

    let parent = current_task().unwrap();
    let exit_signal = match Signals::from_signum((flags & 0xff) as usize) {
        Ok(signal) => signal,
        Err(_) => {
            warn!(
                "[sys_clone] signum of exit_signal is unspecified or invalid: {}",
                (flags & 0xff) as usize
            );
            Signals::empty()
        }
    };
    let flags = CloneFlags::from_bits(flags & !0xff).unwrap();
    if flags.contains(CloneFlags::CLONE_SIGHAND) && !flags.contains(CloneFlags::CLONE_VM) {
        return EINVAL;
    }
    if flags.contains(CloneFlags::CLONE_THREAD) && !flags.contains(CloneFlags::CLONE_SIGHAND) {
        return EINVAL;
    }
    if flags.contains(CloneFlags::CLONE_VFORK) && flags.contains(CloneFlags::CLONE_THREAD) {
        return EINVAL;
    }
    if flags.contains(CloneFlags::CLONE_NEWNS) && flags.contains(CloneFlags::CLONE_FS) {
        return EINVAL;
    }
    if (flags.contains(CloneFlags::CLONE_NEWUTS)
        || flags.contains(CloneFlags::CLONE_NEWNET)
        || flags.contains(CloneFlags::CLONE_NEWNS)
        || flags.contains(CloneFlags::CLONE_NEWIPC))
        && parent.euid() != 0
    {
        return EPERM;
    }
    let mut child: Option<Arc<TaskControlBlock>> = None;
    show_frame_consumption! {
        "clone";
        child = match parent.sys_clone(flags, stack, tls, exit_signal) {
            Ok(task) => Some(task),
            Err(errno) => {
                println!(
                    "[sys_clone] clone failed: errno={} flags={:?} quota={}/{} registry={} free_frames={} heap={}K",
                    errno,
                    flags,
                    crate::task::quota::allocated_task_count(),
                    SYSTEM_TASK_LIMIT,
                    crate::task::ProcessManager::all_processes().len(),
                    crate::mm::unallocated_frames(),
                    crate::mm::heap_stats().1 >> 10,
                );
                return errno;
            }
        };
    }
    let child = match child {
        Some(task) => task,
        None => return ENOMEM,
    };
    let new_tid = child.tid.0;
    if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
        match UserPtrMut::new(ptid).write(current_user_token(), &(new_tid as u32)) {
            Ok(()) => {}
            Err(errno) => {
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return errno;
            }
        };
    }
    if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
        match write_u32_to_task_user(&child, ctid, new_tid as u32) {
            Ok(()) => {}
            Err(errno) => {
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return errno;
            }
        };
    }
    if flags.contains(CloneFlags::CLONE_CHILD_CLEARTID) {
        child.acquire_inner_lock().clear_child_tid = ctid as usize;
    }
    let mut allocated_pidfd = None;
    if flags.contains(CloneFlags::CLONE_PIDFD) {
        let Some(pidfd_ptr) = pidfd_ptr else {
            child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
            return EINVAL;
        };
        let file = match new_pidfd_file(&child.process) {
            Ok(file) => file,
            Err(err) => {
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return -(err as isize);
            }
        };
        let files = parent.process.files();
        let pidfd = match files.lock().alloc_fd(file, false) {
            Ok(fd) => fd,
            Err(err) => {
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return -(err as isize);
            }
        };
        match UserPtrMut::new(pidfd_ptr).write(current_user_token(), &(pidfd as u32)) {
            Ok(()) => allocated_pidfd = Some(pidfd),
            Err(errno) => {
                drop_parent_fd(&parent, pidfd);
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return errno;
            }
        };
    }
    if let Err(errno) = ProcessManager::publish_clone_child(&parent, child.clone(), flags) {
        if let Some(pidfd) = allocated_pidfd {
            drop_parent_fd(&parent, pidfd);
        }
        child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
        return errno;
    }
    ProcessManager::schedule_clone_child(&parent, child, flags);
    new_tid as isize
}
```

这段代码体现了四个顺序约束：flag 校验先于对象构造；`parent.sys_clone()` 只构造 child；`parent_tid/child_tid/pidfd` 写入先于 publish；publish 成功后才 schedule。这个顺序是 clone 失败回滚和 wait 可见性的边界。

## 5. VM 共享与复制

`CLONE_VM` 决定地址空间：

| 情况 | 行为 |
|------|------|
| `CLONE_VM` | `parent_vm.clone()`，父子共享 `Arc<Mutex<AddressSpace>>` |
| 非 `CLONE_VM` | `AddressSpace::from_existing_user()`，fork COW 复制 |

非共享 VM 复制前调用 `frame_reserve(16)`，减少 fork 路径 OOM 概率。

共享 VM 时，child 必须拥有独立 trap context 槽位；非共享 VM 时，子地址空间可以使用父线程同一个 slot 号。

`TaskControlBlock::sys_clone()` 中地址空间分支是 clone 语义的核心：

```rust
let share_vm = flags.contains(CloneFlags::CLONE_VM);
let parent_vm = self.process.vm();
let memory_set = if share_vm {
    parent_vm.clone()
} else {
    crate::mm::frame_reserve(16);
    let parent_trap_cx = *parent_inner.get_trap_cx();
    let copied = AddressSpace::from_existing_user(
        &mut parent_vm.lock(),
        self.user_res_slot,
        &parent_trap_cx,
    )?;
    Arc::new(Mutex::new(copied))
};

let user_res_slot_allocator = if share_vm {
    self.process.user_res_slot_allocator()
} else {
    let allocator = self.process.user_res_slot_allocator();
    let cloned_allocator = allocator.lock().clone();
    Arc::new(Mutex::new(cloned_allocator))
};
let tid_handle = tid_alloc();
let user_res_slot = if share_vm {
    user_res_slot_allocator.lock().alloc()
} else {
    self.user_res_slot
};
let user_stack_allocated =
    !share_vm || (stack.is_null() && !flags.contains(CloneFlags::CLONE_VFORK));
```

共享 VM 时复用同一个 `Arc<Mutex<AddressSpace>>`，但仍分配独立 `user_res_slot`，因为 trap context 虚拟地址处在同一地址空间中。非共享 VM 时通过 `AddressSpace::from_existing_user()` 复制用户地址空间，并沿用当前线程 slot 号；slot 是地址空间内布局索引，不是全局线程 ID。

## 6. PCB 创建与资源共享

`CLONE_THREAD` 直接共享父 PCB；非线程 clone 创建新 PCB。

新 PCB 资源选择：

| 资源 | flag 存在 | flag 不存在 |
|------|-----------|-------------|
| parent | `CLONE_PARENT` 使用父的 parent | 当前进程为 parent |
| files | `CLONE_FILES` 共享 | `FdTable::try_clone()` |
| fs | `CLONE_FS` 共享 | clone `FsStatus` |
| uts | `CLONE_NEWUTS` clone 当前 UTS | 共享当前 UTS |
| net | `CLONE_NEWNET` 新 isolated netns | 共享当前 netns |
| mnt | `CLONE_NEWNS` 新 mount namespace | 共享当前 mnt |
| ipc | `CLONE_NEWIPC` 新 IPC namespace | 共享当前 ipc |
| sighand | `CLONE_SIGHAND` 共享 | `Sighand::from_existing()` |
| futex | `CLONE_VM` 共享 private futex table | 新 `Futex` |

实际 PCB 选择和资源共享在同一个 `let (process, thread_quota)` 表达式内完成：

```rust
let (process, thread_quota) = if flags.contains(CloneFlags::CLONE_THREAD) {
    (self.process.clone(), Some(quota))
} else {
    let parent_process = if flags.contains(CloneFlags::CLONE_PARENT) {
        self.process.parent()
    } else {
        Some(self.process.clone())
    };
    let files = if flags.contains(CloneFlags::CLONE_FILES) {
        self.process.files()
    } else {
        Arc::new(Mutex::new(
            self.process
                .files()
                .lock()
                .try_clone()
                .map_err(|e| e as isize)?,
        ))
    };
    let fs = if flags.contains(CloneFlags::CLONE_FS) {
        self.process.fs()
    } else {
        Arc::new(Mutex::new(self.process.fs().lock().clone()))
    };
    let uts = if flags.contains(CloneFlags::CLONE_NEWUTS) {
        Arc::new(Mutex::new(self.process.uts().lock().clone()))
    } else {
        self.process.uts()
    };
    let net = if flags.contains(CloneFlags::CLONE_NEWNET) {
        NetNamespace::new_isolated()
    } else {
        self.process.net().clone()
    };
    let mnt = if flags.contains(CloneFlags::CLONE_NEWNS) {
        MountNamespace::new()
    } else {
        self.process.mnt()
    };
    let ipc = if flags.contains(CloneFlags::CLONE_NEWIPC) {
        IpcNamespace::new()
    } else {
        self.process.ipc()
    };
    let sighand = if flags.contains(CloneFlags::CLONE_SIGHAND) {
        self.process.sighand()
    } else {
        let sighand = self.process.sighand();
        let lock = sighand.lock();
        Arc::new(Mutex::new(Sighand::from_existing(&lock)))
    };
    let futex = if share_vm {
        self.process.futex()
    } else {
        Arc::new(Mutex::new(Futex::new()))
    };
    (
        Arc::new(ProcessControlBlock::new(
            tid_handle.0,
            tid_handle.0,
            tid_handle.clone(),
            quota,
            self.process.getpgid(),
            self.process.getsid(),
            parent_process.as_ref().map(Arc::downgrade),
            self.process.exe(),
            self.process.exe_path(),
            files,
            fs,
            uts,
            net,
            mnt,
            ipc,
            memory_set.clone(),
            sighand,
            futex,
            user_res_slot_allocator.clone(),
        )),
        None,
    )
};
```

因此 `CLONE_THREAD` 和 `CLONE_VM` 是两层判断：`CLONE_THREAD` 决定是否共享 PCB；`CLONE_VM` 决定是否共享 VM 和 private futex table。非线程 clone 仍可以带 `CLONE_VM`，例如 vfork 类路径。

## 7. child trap context

clone 子任务初始化 trap context：

1. 共享 VM 时从父当前 trap context 复制。
2. 如果 `stack` 非空，设置 child `sp = stack`。
3. `CLONE_SETTLS` 时设置 child `tp = tls`。
4. 设置 child 返回值 `a0 = 0`。
5. 设置 child `kernel_sp = child.kstack_top`。

父进程 syscall 返回 child tid。

## 8. parent_tid、child_tid 与 pidfd

| flag | 写入位置 |
|------|----------|
| `CLONE_PARENT_SETTID` | 父地址空间 `ptid` |
| `CLONE_CHILD_SETTID` | 子地址空间 `ctid` |
| `CLONE_CHILD_CLEARTID` | child inner `clear_child_tid = ctid` |
| `CLONE_PIDFD` | 父 fd table 分配 pidfd，并写 fd 到用户 pidfd ptr |

写父/子用户内存失败时，child 尚未发布，会清理资源并返回对应 errno。

pidfd 分配后若写用户 pidfd pointer 失败，会先从父 fd table drop 已分配 fd，再清理 child。

## 9. clone3 参数

`read_clone3_args()` 校验：

| 条件 | 错误 |
|------|------|
| size 小于 `CloneArgsV0` | `EINVAL` |
| size 大于 `PAGE_SIZE` | `E2BIG` |
| uargs null | `EFAULT` |
| 超出当前支持结构体的额外字节非 0 | `E2BIG` |

`sys_clone3()` 还校验：

| 条件 | 错误 |
|------|------|
| flags 高 32 位非 0 | `EINVAL` |
| exit_signal > 0xff | `EINVAL` |
| pidfd 指针不可写 | `EFAULT` |
| stack 和 stack_size 只给一个 | `EINVAL` |
| stack + stack_size 溢出 | `EINVAL` |

clone3 的 stack 参数按 Linux ABI 转成栈顶地址 `stack + stack_size`。

## 10. vfork

`CLONE_VFORK` 与 `CLONE_THREAD` 不允许同时出现。成功发布 child 后，调度/ProcessManager 会让父线程等待 child 进程的 `vfork_done` completion。

完成条件：

| 子路径 | 调用 |
|--------|------|
| exec 成功 | `task.process.complete_vfork()` |
| exit | `ProcessControlBlock::finish_exit()` 开头 `complete_vfork()` |

因此 vfork 父线程等到 child exec 或 exit 后继续。

## 11. unshare

`sys_unshare(flags)` 支持：

| flag | 行为 |
|------|------|
| `CLONE_FILES` | clone fd table |
| `CLONE_FS` | clone fs status |
| `CLONE_NEWUTS` | clone UTS namespace |
| `CLONE_NEWNET` | 要求 euid 0 且单线程，创建 isolated netns |
| `CLONE_NEWNS` | 要求 euid 0 且单线程，创建 mount namespace |
| `CLONE_NEWIPC` | 要求 euid 0 且单线程，创建 IPC namespace |

传入其他 flag 返回 `EINVAL`。namespace 类 unshare 若非 root 返回 `EPERM`。

## 12. setns

`sys_setns(fd, nstype)` 支持：

| namespace | fd inode 类型 | nstype |
|-----------|---------------|--------|
| net | `ProcNsNetInode` | `0` 或 `CLONE_NEWNET` |
| mnt | `ProcNsMntInode` | `0` 或 `CLONE_NEWNS` |
| ipc | `ProcNsIpcInode` | `0` 或 `CLONE_NEWIPC` |

校验：

| 条件 | 错误 |
|------|------|
| nstype 不是 0/net/mnt/ipc | `EINVAL` |
| fd 无效 | `EBADF` |
| nstype 与 inode 类型不匹配 | `EINVAL` |
| euid 非 0 | `EPERM` |
| fd 不是支持的 ns inode | `EINVAL` |

成功后调用 `set_net/set_mnt/set_ipc()` 替换当前进程 namespace。

## 13. 发布与调度边界

`TaskControlBlock::sys_clone()` 只构造 child，不把非线程 child 加入父进程 children。发布由 `publish_clone_child()` 完成：

| flag | 发布目标 |
|------|----------|
| `CLONE_THREAD` | 不作为 waitable child |
| `CLONE_PARENT` | 加入 child 的 parent 进程 children |
| 默认 | 加入当前进程 children |

发布成功后才调度 child。这样可以在 parent_tid、child_tid、pidfd 等用户态写入失败时安全清理。

发布和调度由两个函数明确隔开：

```rust
pub fn publish_clone_child(
    self: &Arc<TaskControlBlock>,
    child: Arc<TaskControlBlock>,
    flags: CloneFlags,
) -> Result<(), isize> {
    if flags.contains(CloneFlags::CLONE_THREAD) {
        return Ok(());
    }
    if flags.contains(CloneFlags::CLONE_PARENT) {
        let parent = child.process.parent();
        if let Some(parent) = parent {
            parent.add_child(child.process.clone())?;
        } else {
            warn!("[publish_clone_child] CLONE_PARENT target parent is gone");
        }
    } else {
        self.process.add_child(child.process.clone())?;
    }
    Ok(())
}

pub fn schedule_clone_child(
    parent: &Arc<TaskControlBlock>,
    child: Arc<TaskControlBlock>,
    flags: CloneFlags,
) {
    if flags.contains(CloneFlags::CLONE_VFORK) {
        child.process.set_vfork_parent(parent);
        add_task(child.clone());
        child.process.wait_vfork_done_uninterruptible();
    } else {
        add_task(child);
    }
}
```

`CLONE_THREAD` 不加入 children，因此不会作为独立 waitable child；`CLONE_VFORK` 在 child 入队后让父线程等待 child 的 `Completion`，等待点不可中断。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| clone 失败但留下 pidfd | pidfd 写失败路径是否 drop fd |
| 子线程 trap context 覆盖父线程 | 共享 VM 是否分配独立 `user_res_slot` |
| `CLONE_SIGHAND` 行为异常 | 是否同时设置 `CLONE_VM` |
| vfork 父线程不恢复 | child exec/exit 是否调用 `complete_vfork()` |
| unshare net 在多线程进程成功 | `live_thread_count() == 1` 校验 |
