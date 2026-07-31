---
title: "execve 与 execveat"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-31
tags: [process, exec, elf, shebang]
---

# execve 与 execveat

## 1. 源码位置

exec syscall 参数解析在 `os/src/syscall/process/exec.rs`，地址空间替换在 `os/src/task/task.rs::TaskControlBlock::load_elf()`。

```
sys_execve()
  ├── 读取 pathname / argv / envp
  ├── open_exec()
  └── exec_opened_file()

sys_execveat()
  ├── 读取 dirfd/pathname / argv / envp / flags
  ├── open_exec_with_follow() 或 reopen_exec_fd()
  └── exec_opened_file()
        ├── ELF magic 或 shebang
        ├── validate_exec_stack_usage()
        ├── task.load_elf()
        └── mark_execed / set_exe_path / complete_vfork
```

## 2. 路径与文件校验

执行路径校验：

| 条件 | 错误 |
|------|------|
| path 长度 >= `vfs::MAX_PATHLEN` | `ENAMETOOLONG` |
| 任一路径组件长度 > `vfs::NAME_MAX` | `ENAMETOOLONG` |
| `AT_SYMLINK_NOFOLLOW` 且最终 inode 是 symlink | `ELOOP` |
| `execveat` 空 path 但无 `AT_EMPTY_PATH` | `ENOENT` |
| `execveat` 空 path 且 dirfd 为 `AT_FDCWD` | `ENOENT` |

执行文件 metadata 校验：

| 条件 | 错误 |
|------|------|
| inode 不是普通文件且是目录 | `EISDIR` |
| inode 不是普通文件且不是目录 | `EACCES` |
| 根据 fsuid/fsgid/groups 无执行位 | `EACCES` |
| inode 当前被可写打开 | `ETXTBSY` |

root fsuid 仍要求文件至少有任意 execute bit。

## 3. argv/envp 读取

`read_exec_vectors(token, argv, envp)`：

1. 预留 argv/envp Vec 容量。
2. 逐个读取用户指针。
3. 用 `UserCString` 读取字符串。
4. 统计字符串字节数，包含 NUL。
5. argv 为空时补一个空字符串。
6. 总字符串字节数不能超过 `USER_STACK_INIT_SIZE / 2`。

`try_reserve()` 失败返回 `ENOMEM`；大小溢出或超过上限返回 `E2BIG`。

## 4. 栈占用校验

`validate_exec_stack_usage()` 计算最终初始栈占用：

| 内容 | 计入 |
|------|------|
| argv/envp 字符串和 NUL | 是 |
| 对齐 | 是 |
| `AT_RANDOM` 16 字节 | 是 |
| padding | 是 |
| auxv，固定 17 项 | 是 |
| argv/envp 指针数组和 NULL | 是 |
| argc | 是 |

总量必须不超过 `USER_STACK_INIT_SIZE - PAGE_SIZE`，否则 `E2BIG`。

## 5. ELF 与 shebang

`exec_opened_file()` 读取前 4 字节：

| magic | 行为 |
|-------|------|
| `\x7fELF` | 直接执行 |
| `#!` | 解析 shebang |
| 其他 | `ENOEXEC` |

`parse_shebang()` 只读取前 128 字节，取第一行 `#!` 后内容，允许一个可选参数。

shebang 成功时构造 argv：

```text
argv[0] = interpreter
argv[1] = shebang_arg   若存在
argv[next] = script_path
argv[next..] = 原 argv[1..]
```

如果解释器打开失败或不是有效 ELF，会尝试 shell fallback。

## 6. shell fallback

fallback 顺序：

1. `/bin/sh`
2. `/bin/bash`

成功条件是 `open_exec()` 成功且文件 magic 是 ELF。成功后把脚本路径插入 argv[0]，并返回 shell 文件。

`open_exec_with_follow()` 还有兼容分支：当请求 `/bin/sh` 或 `/bin/bash` 失败时，会尝试 `/bash`。

## 7. execveat

支持 flags：

| flag | 行为 |
|------|------|
| `AT_SYMLINK_NOFOLLOW` | 最终路径不跟随 symlink |
| `AT_EMPTY_PATH` | path 为空时执行 dirfd 指向文件 |

其他 flag 返回 `EINVAL`。

非空 path 的起始 inode：

| 条件 | 起点 |
|------|------|
| path 是绝对路径 | 当前工作目录 inode 作为 lookup 根 |
| dirfd 是 `AT_FDCWD` | 当前工作目录 inode |
| dirfd 指向目录 | 该目录 inode |
| dirfd 非目录 | `ENOTDIR` |

空 path 使用 `clone_fd_file(dirfd)` 和 `reopen_exec_fd()` 重新以只读方式打开执行文件。

## 8. load_elf 主流程

`TaskControlBlock::load_elf(elf, argv_vec, envp_vec)`：

1. 目录直接 `EISDIR`。
2. 如果旧 VM 未被其他 `CLONE_VM` 共享，先 `recycle_data_pages()` 降低内存压力。
3. 将 ELF 映射到内核空间 `MMAP_BASE`。
4. `AddressSpaceInner::from_elf()` 构造尚未发布的地址空间数据。
5. 从 `KERNEL_SPACE` 删除临时 ELF 映射。
6. 在新 heap 起点附近预映射 64 KiB 用户 heap。
7. 分配当前线程 user resource 和 trap context。
8. `create_elf_tables()` 写 argv/envp/auxv。
9. 构造新 trap context。
10. 杀掉同线程组其他线程，并从调度队列移除。
11. 更新当前 TCB trap context、清 `clear_child_tid`、重置 robust list、禁用 alt signal stack。
12. 如果旧 VM 共享，移除当前线程在旧 VM 中的 trap context/默认栈映射。
13. `replace_exe()`。
14. 关闭所有 CLOEXEC fd。
15. `replace_vm(memory_set)`。
16. reset sighand。
17. clear futex table。

这个顺序把“可失败的构造”和“不可轻易回滚的替换”尽量分开。ELF 映射、新地址空间、用户资源、argv/envp/auxv 和 trap context 都先在临时对象上完成；只有这些步骤成功后，才杀 sibling 线程、关闭 CLOEXEC fd、替换 VM、重置 sighand 和 futex。这样 exec 失败可以返回 errno，原进程映像仍尽量保持可继续运行。

`load_elf()` 保留当前 TCB/PCB，不创建新进程。exec 改变的是执行映像：VM、trap context、exe inode、部分 signal/futex/fd 状态；PID、父子关系、进程组、会话和大多数进程身份不因 exec 改变。这一点解释了为什么 shell fork 后 child exec，父进程 wait 的仍是同一个 child pid。

`TaskControlBlock::load_elf()` 的核心实现如下。源码先在临时 `AddressSpace` 中完成 ELF、heap、用户栈、auxv 和 trap context 构造；成功后才提交到当前进程：

```rust
pub fn load_elf(
    &self,
    elf: Arc<vfs::File>,
    argv_vec: &Vec<String>,
    envp_vec: &Vec<String>,
) -> Result<(), isize> {
    if elf.is_dir() {
        return Err(EISDIR);
    }
    let current_vm = self.process.vm();
    if Arc::strong_count(&current_vm) <= 2 {
        current_vm.write(|vm| vm.recycle_data_pages());
    }

    let elf_data = elf.map_to_kernel_space(MMAP_BASE);
    if elf_data.is_empty() {
        log::error!("[load_elf] ELF file is empty (size=0)");
        return Err(ENOEXEC);
    }
    let load_result = AddressSpaceInner::from_elf(elf_data);

    crate::mm::KERNEL_SPACE
        .lock()
        .remove_area_with_start_vpn(VirtAddr::from(MMAP_BASE).floor())
        .unwrap();

    let (mut memory_set, program_break, elf_info) = match load_result {
        Ok(result) => result,
        Err(e) => return Err(e),
    };

    use crate::mm::{MapPermission, VirtAddr};
    let page_size = 0x1000;
    let heap_start = align_up(program_break, page_size);
    let heap_end = heap_start + 0x20000;
    memory_set.insert_framed_area(
        VirtAddr::from(heap_start),
        VirtAddr::from(heap_end),
        MapPermission::R | MapPermission::W | MapPermission::U,
    );

    let trap_cx_ppn = memory_set
        .alloc_user_res_with_trap_ppn(self.user_res_slot, true)
        .map_err(|_| ENOMEM)?;
    self.user_stack_allocated.store(true, Ordering::Relaxed);
    let user_sp =
        memory_set.create_elf_tables(self.ustack_bottom_va(), argv_vec, envp_vec, &elf_info)?;
    let trap_cx = TrapContext::app_init_context(
        if let Some(interp_entry) = elf_info.interp_entry {
            interp_entry
        } else {
            elf_info.entry
        },
        user_sp,
        KERNEL_SPACE.lock().token(),
        self.kstack.get_top(),
        trap_handler as usize,
    );

    let other_threads: Vec<_> = self
        .process
        .threads()
        .into_iter()
        .filter(|task| task.tid.0 != self.tid.0)
        .collect();
    for task in &other_threads {
        task.exit_thread_resources(Signals::SIGKILL.to_signum().unwrap() as u32);
    }
    super::remove_tasks_from_queues(&other_threads);

    {
        let mut inner = self.acquire_inner_lock();
        inner.trap_cx_ppn = trap_cx_ppn;
        *inner.get_trap_cx() = trap_cx;
        inner.clear_child_tid = 0;
        inner.robust_list = RobustList::default();
        inner.signal_stack = SignalStack::disabled();
    }
    if Arc::strong_count(&current_vm) > 2 {
        current_vm.write(|vm| {
            vm.dealloc_user_res_with_stack(
                self.user_res_slot,
                self.user_stack_allocated.load(Ordering::Relaxed),
            );
        });
    }
    self.process.replace_exe(elf);
    {
        let files_ref = self.process.files();
        let mut fd_table = files_ref.lock();
        crate::syscall::fs::close_cloexec_and_release_fcntl_locks(self.pid(), &mut fd_table);
    }
    self.process.replace_vm(memory_set);
    self.process.sighand().lock().reset();
    self.process.futex().lock().clear();
    Ok(())
}
```

这段实现的提交点从 `other_threads` 退出开始。在此之前，失败以 `Err(errno)` 返回给 exec 上层；之后当前执行映像已经开始切换，不能再按原进程映像完整回滚。

## 9. 多线程 exec 语义

exec 成功后保留当前任务和当前 PCB，但杀掉同线程组其他线程：

```text
other_threads = process.threads().filter(tid != current)
for task in other_threads:
    task.exit_thread_resources(SIGKILL)
remove_tasks_from_queues(other_threads)
```

这不是 `exit_group()`；进程仍继续运行，只是线程组收缩到执行 exec 的线程。

> **B41 前置限制：** 上述代码仍是当前实现的旧路径，不是 SMP 安全协议。
> `exit_thread_resources()` 不能由 exec 发起 CPU 代替远端 `Running` sibling 执行；
> `remove_tasks_from_queues()` 也不能证明远端 current 已切离旧 MM。B41 必须先请求
> sibling 在各自 owner 安全点停止并发布 completion，exec 发起者收到全部 ack 后，
> 才能替换地址空间和继续执行。B40 的永久 group-exit gate 不能直接复用，因为 exec
> 发起线程必须存活并重新开放线程创建。

## 10. VM 共享场景

如果旧 VM 被共享，例如 `CLONE_VM | CLONE_VFORK`，exec 不能先破坏父 VM。代码先构造新地址空间，提交时再：

```text
current_vm.dealloc_user_res_with_stack(current_slot, current_user_stack_allocated)
process.replace_vm(new_memory_set)
```

这样 vfork child exec 后脱离父 VM，父 VM 中不留下该 child 的 trap context/默认栈映射。

## 11. CLOEXEC、sighand、futex

exec 成功后：

| 资源 | 行为 |
|------|------|
| fd table | 关闭 `O_CLOEXEC` fd，并释放 fcntl locks |
| sighand | `reset()`，恢复默认 signal action |
| futex | `clear()`，清空 private futex table |
| alt signal stack | disabled |
| robust list | reset |
| clear_child_tid | 清 0 |

## 12. vfork 完成

`exec_opened_file()` 成功调用 `task.load_elf()` 后：

```rust
task.process.mark_execed();
task.process.set_exe_path(abs_path);
task.process.complete_vfork();
```

因此 vfork 父线程在 child exec 成功后被唤醒。

## 13. 失败边界

如果 `load_elf()` 返回错误，该路径会：

```rust
exit_current_and_run_next(127);
```

这意味着 ELF 加载阶段失败不是简单返回 errno 给用户态，而是让当前任务以 127 退出。执行前的 open/metadata/argv/envp 校验失败才直接返回 errno。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| 脚本不能执行 | shebang 前 128 字节、解释器 ELF、shell fallback |
| exec 大参数返回 E2BIG | 字符串总量和最终栈占用两层限制 |
| exec 后旧 fd 未关闭 | `close_cloexec_and_release_fcntl_locks()` |
| exec 后 signal handler 仍旧 | `sighand.reset()` |
| vfork 父线程卡住 | `complete_vfork()` 是否在成功路径调用 |
